param([ValidateSet('aarch64')][string]$Target = 'aarch64', [switch]$VerifyOnly)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$signing = Join-Path $project '.release-signing'
$keystore = Join-Path $signing 'nuvio-android.jks'
$passwordFile = Join-Path $signing 'android-password.txt'
$legacyPasswordFile = Join-Path $signing 'android-password.dpapi'
New-Item -ItemType Directory -Force -Path $signing | Out-Null
if (-not (Test-Path -LiteralPath $passwordFile) -and (Test-Path -LiteralPath $legacyPasswordFile)) {
    $legacySecure = (Get-Content -LiteralPath $legacyPasswordFile -Raw).Trim() | ConvertTo-SecureString
    [IO.File]::WriteAllText($passwordFile, [Net.NetworkCredential]::new('', $legacySecure).Password)
}
if (-not (Test-Path -LiteralPath $passwordFile)) {
    if (Test-Path -LiteralPath $keystore) { throw 'Existe una firma sin contraseña guardada; recupera su copia de seguridad.' }
    $random = New-Object byte[] 32
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    $rng.GetBytes($random)
    $rng.Dispose()
    [IO.File]::WriteAllText($passwordFile, [Convert]::ToBase64String($random))
}
# The private signing folder is excluded from Git, Vite and all distribution packages.
# Keep this folder private and back it up: updates must use the same signing key.
$password = (Get-Content -LiteralPath $passwordFile -Raw).Trim()
$env:NUVIO_ANDROID_KEY_PASSWORD = $password
$env:NUVIO_ANDROID_KEYSTORE = $keystore
try {
    if (-not (Test-Path -LiteralPath $keystore)) {
        $javaDir = Get-ChildItem -LiteralPath (Join-Path $env:USERPROFILE 'Java') -Directory | Where-Object Name -Like 'jdk-17*' | Select-Object -First 1
        if (-not $javaDir) { throw 'Se necesita JDK 17 para crear la firma de Android.' }
        & (Join-Path $javaDir.FullName 'bin/keytool.exe') -genkeypair -keystore $keystore -storetype JKS -alias nuvio -storepass:env NUVIO_ANDROID_KEY_PASSWORD -keypass:env NUVIO_ANDROID_KEY_PASSWORD -keyalg RSA -keysize 3072 -validity 10000 -dname 'CN=Nuvio, O=Nuvio, C=MX' -noprompt
        if ($LASTEXITCODE -ne 0) { throw 'No se pudo crear la firma de distribución.' }
    }
    if (-not $VerifyOnly) {
        Push-Location $project
        try {
            & node scripts/tauri-android.mjs build --apk --target $Target --ci
            if ($LASTEXITCODE -ne 0) { throw 'La compilación Android no terminó correctamente.' }
        } finally { Pop-Location }
    }
    $sdk = if ($env:ANDROID_HOME) {
        $env:ANDROID_HOME
    } elseif ($env:ANDROID_SDK_ROOT) {
        $env:ANDROID_SDK_ROOT
    } elseif ($env:LOCALAPPDATA) {
        Join-Path $env:LOCALAPPDATA 'Android/Sdk'
    } else {
        Join-Path $env:USERPROFILE 'AppData/Local/Android/Sdk'
    }
    $buildTools = Get-ChildItem -LiteralPath (Join-Path $sdk 'build-tools') -Directory | Where-Object Name -Match '^\d+\.\d+\.\d+$' | Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
    $javaDir = Get-ChildItem -LiteralPath (Join-Path $env:USERPROFILE 'Java') -Directory | Where-Object Name -Like 'jdk-17*' | Select-Object -First 1
    if (-not $buildTools -or -not $javaDir) { throw 'Faltan las herramientas de firma/verificación del SDK/JDK.' }
    $java = Join-Path $javaDir.FullName 'bin/java.exe'
    $apksigner = Join-Path $buildTools.FullName 'lib/apksigner.jar'
    $zipalign = Join-Path $buildTools.FullName 'zipalign.exe'

    $apkRoot = Join-Path $project 'src-tauri/gen/android/app/build/outputs/apk'
    $releaseApk = Join-Path $project "release/Android/Nuvio-Android-$Target.apk"
    $apkCandidates = @()
    if (Test-Path -LiteralPath $apkRoot) {
        $apkCandidates += Get-ChildItem -LiteralPath $apkRoot -Recurse -Filter '*.apk' -File | Where-Object { $_.Name -notlike '*androidTest*' }
    }
    if ($VerifyOnly -and (Test-Path -LiteralPath $releaseApk)) {
        $apkCandidates += Get-Item -LiteralPath $releaseApk
    }
    $apk = $apkCandidates | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $apk) { throw 'No se encontró ningún APK release para firmar/verificar.' }

    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $java -jar $apksigner verify --verbose $apk.FullName 1>$null 2>$null
        $alreadySigned = $LASTEXITCODE -eq 0
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    if (-not $alreadySigned) {
        $signingOutput = Join-Path $project 'qa/android-signing'
        New-Item -ItemType Directory -Force -Path $signingOutput | Out-Null
        $alignedApk = Join-Path $signingOutput "Nuvio-Android-$Target-aligned.apk"
        $signedApk = Join-Path $signingOutput "Nuvio-Android-$Target-signed.apk"
        Remove-Item -LiteralPath $alignedApk,$signedApk -Force -ErrorAction SilentlyContinue
        & $zipalign -f -P 16 4 $apk.FullName $alignedApk
        if ($LASTEXITCODE -ne 0) { throw 'No se pudo alinear el APK antes de firmarlo.' }
        & $java -jar $apksigner sign --ks $keystore --ks-key-alias nuvio --ks-pass 'env:NUVIO_ANDROID_KEY_PASSWORD' --key-pass 'env:NUVIO_ANDROID_KEY_PASSWORD' --out $signedApk $alignedApk
        if ($LASTEXITCODE -ne 0) { throw 'No se pudo firmar el APK de distribución.' }
        $apk = Get-Item -LiteralPath $signedApk
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($apk.FullName)
    $elfDirectory = Join-Path $project 'qa/android-elf'
    New-Item -ItemType Directory -Force -Path $elfDirectory | Out-Null
    try {
        foreach ($library in @('lib/arm64-v8a/libnuviodrive_v1_lib.so', 'lib/arm64-v8a/libtdjson.so')) {
            if (-not $archive.GetEntry($library)) { throw "El APK está incompleto: falta $library" }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($archive.GetEntry($library), (Join-Path $elfDirectory (Split-Path $library -Leaf)), $true)
        }
    } finally { $archive.Dispose() }
    $ndk = if ($env:NDK_HOME) { Get-Item -LiteralPath $env:NDK_HOME } else { Get-ChildItem -LiteralPath (Join-Path $sdk 'ndk') -Directory | Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1 }
    $readelf = Join-Path $ndk.FullName 'toolchains/llvm/prebuilt/windows-x86_64/bin/llvm-readelf.exe'
    $elfReport = @()
    foreach ($library in @('libnuviodrive_v1_lib.so', 'libtdjson.so')) {
        $details = & $readelf -W -h -l -d (Join-Path $elfDirectory $library)
        if ($LASTEXITCODE -ne 0) { throw "No se pudo analizar $library" }
        $elfReport += $library
        $elfReport += $details
        if (($details -join "`n") -notmatch 'Machine:\s+AArch64') { throw "Arquitectura incorrecta en $library" }
        if (($details -join "`n") -notmatch 'Shared library: \[libc\.so\]') { throw "$library debe usar la libc de Android; no se admite enlazar libc.a." }
        foreach ($line in $details) {
            if ($line -match '^\s+LOAD\s+.*\s+(0x[0-9a-fA-F]+)\s*$' -and [Convert]::ToInt64($Matches[1].Substring(2), 16) -lt 16384) { throw "$library no admite páginas de 16 KB." }
        }
    }
    $elfReport | Set-Content -LiteralPath (Join-Path $project 'qa/android-elf.log')
    & $java -jar $apksigner verify --verbose --print-certs $apk.FullName *> (Join-Path $project 'qa/android-signature.log')
    if ($LASTEXITCODE -ne 0) { throw 'El APK no superó la verificación de firma.' }
    & $zipalign -c -P 16 -v 4 $apk.FullName *> (Join-Path $project 'qa/android-alignment.log')
    if ($LASTEXITCODE -ne 0) { throw 'El APK no está alineado para Android con páginas de 16 KB.' }
    & (Join-Path $buildTools.FullName 'aapt.exe') dump badging $apk.FullName *> (Join-Path $project 'qa/android-manifest.log')
    if ($LASTEXITCODE -ne 0) { throw 'No se pudo verificar el manifiesto del APK.' }
    $manifest = Get-Content -LiteralPath (Join-Path $project 'qa/android-manifest.log') -Raw
    if ($manifest -match 'application-debuggable') { throw 'El APK de distribución no debe ser depurable.' }
    if ($manifest -notmatch "uses-permission: name='android.permission.POST_NOTIFICATIONS'") { throw 'El APK no declara POST_NOTIFICATIONS para Android 13+.' }
    if ($manifest -notmatch "native-code:.*'arm64-v8a'") { throw 'El APK no incluye la arquitectura ARM64 del S24 Ultra.' }
    $output = Join-Path $project 'release/Android'
    New-Item -ItemType Directory -Force -Path $output | Out-Null
    $destinationApk = Join-Path $output "Nuvio-Android-$Target.apk"
    if (-not [IO.Path]::GetFullPath($apk.FullName).Equals([IO.Path]::GetFullPath($destinationApk), [StringComparison]::OrdinalIgnoreCase)) {
        Copy-Item -LiteralPath $apk.FullName -Destination $destinationApk -Force
    }
    Write-Output "APK firmado: $destinationApk"
} finally {
    $env:NUVIO_ANDROID_KEY_PASSWORD = $null
    $password = $null
}
