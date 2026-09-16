<#
.SYNOPSIS
    Cuts a release: bumps the version, builds the installer, tags it and
    publishes a GitHub release with the installer attached.

.DESCRIPTION
    Everything happens on this machine -- no CI. The steps, in order:

      1. check the working tree, the remote and that gh is signed in
      2. work out the next version and confirm it with you
      3. bump the version in Cargo.toml
      4. build the installer
      5. commit everything as "release for version x.y.z", tag it, push both
      6. create the GitHub release and upload the installer

    If anything fails after the version bump, the bump is rolled back so the
    tree is left as it was found.

    The default version is a guess from the commits since the last tag, using
    Conventional Commit prefixes: a `!` marker or a BREAKING CHANGE footer means
    major, a `feat` means minor, anything else patch. With no tags yet, the
    version already in Cargo.toml is offered as the first release.

.PARAMETER Version
    Use this exact version instead of the predicted one.

.PARAMETER Bump
    Force the size of the bump: major, minor or patch.

.PARAMETER Draft
    Create the release as a draft.

.PARAMETER PreRelease
    Mark the release as a pre-release.

.PARAMETER Notes
    Release notes. Without this, GitHub generates them from the commits.

.PARAMETER SkipBuild
    Reuse the installer already in dist\ instead of rebuilding it.

.PARAMETER SkipTests
    Pass -SkipTests through to the build.

.PARAMETER DryRun
    Print every step without changing, tagging, pushing or publishing anything.

.PARAMETER Yes
    Do not prompt. Uses the predicted version unless -Version is given.

.EXAMPLE
    scripts\publish.ps1

.EXAMPLE
    scripts\publish.ps1 -Bump minor

.EXAMPLE
    scripts\publish.ps1 -DryRun
#>
[CmdletBinding()]
param(
    [string]$Version,
    [ValidateSet('major', 'minor', 'patch')] [string]$Bump,
    [switch]$Draft,
    [switch]$PreRelease,
    [string]$Notes,
    [switch]$SkipBuild,
    [switch]$SkipTests,
    [switch]$DryRun,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'

if (-not $PSScriptRoot) {
    throw 'Run this as a script file, not by pasting its contents.'
}
. (Join-Path $PSScriptRoot 'common.ps1')

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $Root

function Invoke-GitQuiet {
    param([Parameter(ValueFromRemainingArguments)] [string[]]$Arguments = @())
    return Invoke-Quiet -Exe 'git' -Arguments $Arguments
}

# ---------------------------------------------------------------------------
# Version arithmetic
# ---------------------------------------------------------------------------

function Step-Version {
    param([string]$Current, [string]$Level)

    if ($Current -notmatch '^(\d+)\.(\d+)\.(\d+)') {
        Stop-WithMessage "Cargo.toml version '$Current' is not a plain x.y.z version; pass -Version instead."
    }
    $major = [int]$Matches[1]
    $minor = [int]$Matches[2]
    $patch = [int]$Matches[3]

    switch ($Level) {
        'major' { return "$($major + 1).0.0" }
        'minor' { return "$major.$($minor + 1).0" }
        default { return "$major.$minor.$($patch + 1)" }
    }
}

<#
    Guesses the size of the next bump from the commits since $SinceTag, reading
    Conventional Commit prefixes. Falls back to patch, which is the safe guess.
#>
function Get-BumpLevel {
    param([string]$SinceTag)

    $range = if ($SinceTag) { "$SinceTag..HEAD" } else { 'HEAD' }
    $subjects = @((Invoke-GitQuiet log $range --pretty=format:%s).Text -split "`r?`n")
    $bodies = @((Invoke-GitQuiet log $range --pretty=format:%b).Text -split "`r?`n")

    if ($bodies -match 'BREAKING[ -]CHANGE') { return 'major' }
    foreach ($subject in $subjects) {
        # feat!: ... or fix(scope)!: ...
        if ($subject -match '^[a-zA-Z]+(\([^)]*\))?!:') { return 'major' }
    }
    foreach ($subject in $subjects) {
        if ($subject -match '^feat(\([^)]*\))?:') { return 'minor' }
    }
    return 'patch'
}

# ---------------------------------------------------------------------------
# Checks
# ---------------------------------------------------------------------------

Write-Head 'Rosetta - publish'
if ($DryRun) { Write-Warn 'Dry run: nothing will be changed, pushed or published.' }

foreach ($tool in 'git', 'gh', 'cargo') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        Stop-WithMessage "$tool was not found on your PATH."
    }
}

if (-not (Invoke-GitQuiet rev-parse --is-inside-work-tree).Ok) {
    Stop-WithMessage 'This is not a git repository.'
}
if (-not (Invoke-Quiet -Exe 'gh' -Arguments @('auth', 'status')).Ok) {
    Stop-WithMessage 'gh is not signed in. Run "gh auth login" and try again.'
}

$remoteResult = Invoke-GitQuiet remote get-url origin
if (-not $remoteResult.Ok -or -not $remoteResult.Text) {
    Stop-WithMessage 'No "origin" remote. Add one with: git remote add origin <url>'
}
$remote = $remoteResult.Text
if ($remote -notmatch 'github\.com') {
    Stop-WithMessage "origin does not look like GitHub: $remote"
}

$branch = (Invoke-GitQuiet branch --show-current).Text
if (-not $branch) {
    Stop-WithMessage 'HEAD is detached. Check out a branch before publishing.'
}

# Whatever is in the tree is what gets released, so it all goes into the release
# commit. It is listed below before anything happens, because `git add -A` also
# picks up untracked files.
$pending = @((Invoke-GitQuiet status --porcelain).Text -split "`r?`n" | Where-Object { $_ })

# ---------------------------------------------------------------------------
# Work out the version
# ---------------------------------------------------------------------------

$current = Get-ProjectVersion -Root $Root
$describe = Invoke-GitQuiet describe --tags --abbrev=0
$lastTag = if ($describe.Ok) { $describe.Text } else { $null }

if ($Version) {
    $target = $Version -replace '^v', ''
    $reason = 'given with -Version'
}
elseif ($Bump) {
    $target = Step-Version -Current $current -Level $Bump
    $reason = "forced $Bump bump"
}
elseif (-not $lastTag) {
    # Nothing released yet, so what is in Cargo.toml has never shipped.
    $target = $current
    $reason = 'first release, using the version already in Cargo.toml'
}
else {
    $level = Get-BumpLevel -SinceTag $lastTag
    $target = Step-Version -Current $current -Level $level
    $reason = "$level bump, guessed from the commits since $lastTag"
}

if ($target -notmatch '^\d+\.\d+\.\d+') {
    Stop-WithMessage "'$target' is not a valid version."
}

$tag = "v$target"
if ((Invoke-GitQuiet tag --list $tag).Text) {
    Stop-WithMessage "Tag $tag already exists. Pass -Version with something newer."
}

$commitCount = if ($lastTag) {
    (Invoke-GitQuiet rev-list --count "$lastTag..HEAD").Text
}
else {
    (Invoke-GitQuiet rev-list --count HEAD).Text
}

Write-Host "  Repository   " -NoNewline; Write-Host $remote -ForegroundColor Green
Write-Host "  Branch       " -NoNewline; Write-Host $branch -ForegroundColor Green
Write-Host "  Last tag     " -NoNewline; Write-Host $(if ($lastTag) { $lastTag } else { '(none)' }) -ForegroundColor Green
Write-Host "  Commits      " -NoNewline; Write-Host "$commitCount since then" -ForegroundColor Green
Write-Host "  Version      " -NoNewline; Write-Host "$current -> $target" -ForegroundColor Green
Write-Step "($reason)"

if ($pending.Count -gt 0) {
    Write-Host ''
    Write-Host "  These $($pending.Count) change(s) will go into the release commit:" -ForegroundColor Yellow
    $pending | Select-Object -First 20 | ForEach-Object {
        Write-Host "    $_" -ForegroundColor DarkGray
    }
    if ($pending.Count -gt 20) {
        Write-Host "    ... and $($pending.Count - 20) more" -ForegroundColor DarkGray
    }
}
Write-Host ''

if (-not $Yes -and -not $DryRun) {
    $answer = Read-Host "  Version to release [$target]"
    if ($answer) {
        $target = $answer -replace '^v', ''
        if ($target -notmatch '^\d+\.\d+\.\d+') {
            Stop-WithMessage "'$target' is not a valid version."
        }
        $tag = "v$target"
        if ((Invoke-GitQuiet tag --list $tag).Text) {
            Stop-WithMessage "Tag $tag already exists."
        }
    }
    $confirm = Read-Host "  Build $tag, tag it and publish to GitHub? [y/N]"
    if ($confirm -notmatch '^(y|yes)$') {
        Write-Host ''
        Write-Step 'Nothing done.'
        Write-Host ''
        exit 0
    }
}

$installer = Get-InstallerPath -Root $Root -Version $target
$commitMessage = "release for version $target"

# ---------------------------------------------------------------------------
# Do it
# ---------------------------------------------------------------------------

if ($DryRun) {
    Write-Head 'Would run'
    Write-Step "set Cargo.toml version to $target"
    if (-not $SkipBuild) { Write-Step "scripts\package.ps1$(if ($SkipTests) { ' -SkipTests' })" }
    Write-Step 'git add -A'
    Write-Step "git commit -m '$commitMessage'"
    Write-Step "git tag -a $tag -m 'Rosetta $tag'"
    Write-Step "git push origin $branch"
    Write-Step "git push origin $tag"
    Write-Step "gh release create $tag '$installer' ..."
    Write-Host ''
    exit 0
}

$lockPath = Join-Path $Root 'Cargo.lock'
$lockBefore = if (Test-Path -LiteralPath $lockPath) { Get-Content -Raw -LiteralPath $lockPath } else { $null }

$bumped = $false
try {
    if ($target -ne $current) {
        Write-Step "Setting version to $target..."
        Set-ProjectVersion -Root $Root -Version $target
        $bumped = $true
        # Keep Cargo.lock in step; the build would do it anyway, but the lock
        # belongs in the same commit as the manifest.
        #
        # Through Invoke-Quiet, not Invoke-Checked: cargo reports ordinary
        # progress ("Locking 1 package...") on stderr, and merging that into the
        # pipeline while ErrorActionPreference is Stop turns a successful run
        # into a terminating NativeCommandError.
        Write-Step 'Updating Cargo.lock...'
        $lock = Invoke-Quiet -Exe 'cargo' -Arguments @('update', '--offline', '--package', 'rosetta-desktop')
        if (-not $lock.Ok) {
            Write-Warn 'Could not refresh Cargo.lock here; the build will do it.'
        }
    }

    if ($SkipBuild) {
        if (-not (Test-Path -LiteralPath $installer)) {
            Stop-WithMessage "-SkipBuild was given but $installer does not exist."
        }
        Write-Warn 'Reusing the existing installer (-SkipBuild).'
    }
    else {
        Write-Step 'Building the installer (this takes a few minutes)...'
        # Named, not splatted: see the note in package.ps1.
        & (Join-Path $PSScriptRoot 'package.ps1') -SkipTests:$SkipTests
        if ($LASTEXITCODE -ne 0) {
            Stop-WithMessage 'The build failed. Nothing has been tagged or pushed.'
        }
    }

    if (-not (Test-Path -LiteralPath $installer)) {
        Stop-WithMessage "Expected $installer but it is not there."
    }

    Invoke-Checked 'Staging everything...' { git add -A }

    # Exit code 1 means "there are staged changes", which is the normal case
    # here, so it must not be read as a failure.
    $staged = Invoke-Quiet -Exe 'git' -Arguments @('diff', '--cached', '--quiet')
    if ($staged.Ok) {
        Write-Step 'Nothing to commit; tagging the current commit.'
    }
    else {
        Invoke-Checked "Committing as '$commitMessage'..." { git commit -m $commitMessage }
    }

    Invoke-Checked "Tagging $tag..." { git tag -a $tag -m "Rosetta $tag" }
    Invoke-Checked "Pushing $branch..." { git push origin $branch }
    Invoke-Checked "Pushing $tag..." { git push origin $tag }
    $bumped = $false   # committed and pushed; there is nothing left to undo
}
finally {
    # This must be finally, not catch: every failure above leaves through
    # Stop-WithMessage, which calls exit, and exit skips catch but still runs
    # finally.
    if ($bumped) {
        Write-Warn "Rolling the version back to $current."
        Set-ProjectVersion -Root $Root -Version $current
        # The lock may already carry the new version, so put it back too.
        if ($null -ne $lockBefore) {
            Set-Content -LiteralPath $lockPath -Value $lockBefore -NoNewline
        }
    }
}

$releaseArgs = @('release', 'create', $tag, '--title', "Rosetta $tag")
if ($Notes) { $releaseArgs += @('--notes', $Notes) } else { $releaseArgs += '--generate-notes' }
if ($Draft) { $releaseArgs += '--draft' }
if ($PreRelease) { $releaseArgs += '--prerelease' }
$releaseArgs += $installer

Invoke-Checked 'Creating the GitHub release...' { gh @releaseArgs }

$url = (Invoke-Quiet -Exe 'gh' -Arguments @('release', 'view', $tag, '--json', 'url', '--jq', '.url')).Text

Write-Host ''
Write-Good "Published $tag"
if ($url) { Write-Host "  $url" -ForegroundColor Green }
Write-Step "$(Format-Size (Get-Item -LiteralPath $installer).Length) installer attached"
Write-Host ''
