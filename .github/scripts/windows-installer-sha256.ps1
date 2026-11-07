function Assert-ArchiveSha256($Path, $Expected) {
  if (-not $Expected) {
    throw "ERROR: no expected sha256 for the downloaded archive"
  }
  # Stop on a missing file: a non-terminating error would leave $actual empty and skip the compare.
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path -ErrorAction Stop).Hash
  if ($actual -ne $Expected) {
    throw "ERROR: sha256 mismatch for the downloaded archive: expected $Expected, got $actual"
  }
}
