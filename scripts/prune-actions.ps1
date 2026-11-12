[CmdletBinding()]
param([int]$Keep = 3, [string[]]$Workflow = @('release.yml', 'actions-retention.yml'), [switch]$DryRun)
$ErrorActionPreference = 'Stop'
if ($Keep -lt 1) { throw 'Keep must be positive.' }
$repo = $env:GITHUB_REPOSITORY
if ($repo -notmatch '^[\w.-]+/[\w.-]+$') { throw 'GITHUB_REPOSITORY is required.' }
foreach ($file in $Workflow) {
    if ($file -notmatch '^[\w.-]+\.ya?ml$') { throw 'Invalid workflow filename.' }
    $json = & gh api --paginate --slurp "repos/$repo/actions/workflows/$file/runs?per_page=100"
    if ($LASTEXITCODE -ne 0) { throw "Cannot list workflow runs: $file" }
    $runs = @($json | ConvertFrom-Json | ForEach-Object { $_.workflow_runs } | Sort-Object created_at,id -Descending)
    $obsolete = @($runs | Select-Object -Skip $Keep | Where-Object { $_.status -eq 'completed' -and [string]$_.id -ne $env:GITHUB_RUN_ID })
    foreach ($run in $obsolete) {
        Write-Host "Prune $file run $($run.id) ($($run.created_at)); release assets and tags are preserved."
        if (-not $DryRun) {
            & gh api --method DELETE "repos/$repo/actions/runs/$($run.id)"
            if ($LASTEXITCODE -ne 0) { throw "Cannot delete workflow run $($run.id)" }
        }
    }
    Write-Host "$file`: keeping the latest $Keep runs; removed $($obsolete.Count) completed runs$(if($DryRun){' (dry run)'})"
}
