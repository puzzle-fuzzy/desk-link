$ErrorActionPreference = 'Stop'
& python (Join-Path $PSScriptRoot 'verify.py') @args
exit $LASTEXITCODE
