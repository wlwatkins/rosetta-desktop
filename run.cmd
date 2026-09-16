@echo off
rem Double-click target. Windows opens .ps1 files in an editor rather than
rem running them, so this wrapper launches the menu properly.
rem
rem   run.cmd                     menu: run, build, package, publish, set up
rem   run.cmd run                 launch the tray app (release)
rem   run.cmd run -Log            launch with scan logging
rem   run.cmd build               check, test and build release
rem   run.cmd package             build the installer into dist
rem   run.cmd setup               one-time: fetch the model and Tesseract
rem   run.cmd bench               compare int8/fp32 and greedy/beam
rem   run.cmd publish             bump, tag and release to GitHub
rem
rem All argument handling lives in scripts\menu.ps1; batch just forwards.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\menu.ps1" %*
