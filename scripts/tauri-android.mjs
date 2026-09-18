import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const [command = "build", ...forwardedArgs] = process.argv.slice(2);
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(scriptDirectory, "..");
const buildLockPath = join(projectRoot, ".nuvio-android-build.lock");

function processIsAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

function acquireBuildLock() {
  if (command !== "build") return () => {};

  if (existsSync(buildLockPath)) {
    const previousPid = Number.parseInt(readFileSync(buildLockPath, "utf8").trim(), 10);
    if (processIsAlive(previousPid)) {
      throw new Error(
        `Ya hay una compilación Android de Nuvio en curso (PID ${previousPid}). Espera a que termine antes de iniciar otra.`,
      );
    }
    unlinkSync(buildLockPath);
  }

  writeFileSync(buildLockPath, String(process.pid), { flag: "wx" });
  let released = false;
  const release = () => {
    if (released) return;
    released = true;
    try {
      if (existsSync(buildLockPath)) unlinkSync(buildLockPath);
    } catch {
      // El sistema operativo también libera el proceso; el lock obsoleto se limpia en el siguiente inicio.
    }
  };
  process.once("exit", release);
  process.once("SIGINT", () => {
    release();
    process.exit(130);
  });
  process.once("SIGTERM", () => {
    release();
    process.exit(143);
  });
  return release;
}

function existingDirectory(candidates) {
  return candidates.find((candidate) => candidate && existsSync(candidate));
}

function versionParts(value) {
  return value.split(/[.-]/).map((part) => Number.parseInt(part, 10) || 0);
}

function compareVersionsDesc(left, right) {
  const a = versionParts(left);
  const b = versionParts(right);
  const length = Math.max(a.length, b.length);
  for (let index = 0; index < length; index += 1) {
    const delta = (b[index] ?? 0) - (a[index] ?? 0);
    if (delta !== 0) return delta;
  }
  return 0;
}

function detectSdkRoot() {
  const defaults = process.platform === "win32"
    ? [
        process.env.LOCALAPPDATA && join(process.env.LOCALAPPDATA, "Android", "Sdk"),
        join(homedir(), "AppData", "Local", "Android", "Sdk"),
      ]
    : process.platform === "darwin"
      ? [join(homedir(), "Library", "Android", "sdk")]
      : [join(homedir(), "Android", "Sdk")];

  return existingDirectory([
    process.env.ANDROID_HOME,
    process.env.ANDROID_SDK_ROOT,
    ...defaults,
  ]);
}

function detectNdkRoot() {
  const explicit = existingDirectory([
    process.env.NDK_HOME,
    process.env.ANDROID_NDK_HOME,
  ]);
  if (explicit) return explicit;

  const sdkRoot = detectSdkRoot();
  if (!sdkRoot) return null;
  const ndkDirectory = join(sdkRoot, "ndk");
  if (!existsSync(ndkDirectory)) return null;

  const versions = readdirSync(ndkDirectory, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort(compareVersionsDesc);

  return versions.length ? join(ndkDirectory, versions[0]) : null;
}

function javaMajor(javaHome) {
  if (!javaHome) return null;
  const java = join(javaHome, "bin", process.platform === "win32" ? "java.exe" : "java");
  if (!existsSync(java)) return null;
  const result = spawnSync(java, ["-version"], { encoding: "utf8", shell: false });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  const match = output.match(/version\s+"(\d+)/i);
  return match ? Number.parseInt(match[1], 10) : null;
}

function childDirectories(path) {
  if (!path || !existsSync(path)) return [];
  return readdirSync(path, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => join(path, entry.name));
}

function detectJava17() {
  const candidates = [process.env.JAVA_HOME];

  const userJava = join(homedir(), "Java");
  candidates.push(
    ...childDirectories(userJava)
      .filter((path) => /jdk-?17/i.test(path))
      .sort()
      .reverse(),
  );

  if (process.platform === "win32") {
    const programFiles = process.env.ProgramFiles || "C:\\Program Files";
    candidates.push(
      ...childDirectories(join(programFiles, "Eclipse Adoptium"))
        .filter((path) => /jdk-?17/i.test(path))
        .sort()
        .reverse(),
    );
  }

  return candidates.find((candidate) => javaMajor(candidate) === 17) ?? null;
}

function androidCppDirectories(ndkRoot) {
  const prebuiltRoot = join(ndkRoot, "toolchains", "llvm", "prebuilt");
  if (!existsSync(prebuiltRoot)) {
    throw new Error(`NDK inválido: no existe ${prebuiltRoot}`);
  }

  const hosts = readdirSync(prebuiltRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name);
  if (!hosts.length) throw new Error("El NDK no contiene un toolchain LLVM prebuilt");

  const sysrootLib = join(prebuiltRoot, hosts[0], "sysroot", "usr", "lib");
  const targets = {
    "aarch64-linux-android": "aarch64-linux-android",
    "armv7-linux-androideabi": "arm-linux-androideabi",
    "i686-linux-android": "i686-linux-android",
    "x86_64-linux-android": "x86_64-linux-android",
  };

  return Object.fromEntries(
    Object.entries(targets).flatMap(([cargoTarget, ndkTarget]) => {
      const libDir = join(sysrootLib, ndkTarget);
      return existsSync(join(libDir, "libc++_static.a"))
        ? [[cargoTarget, libDir]]
        : [];
    }),
  );
}

function writeCargoAndroidConfig(ndkRoot) {
  const directories = androidCppDirectories(ndkRoot);
  const cargoDirectory = join(projectRoot, "src-tauri", ".cargo");
  const configPath = join(cargoDirectory, "config.toml");
  const marker = "# Generated by Nuvio scripts/tauri-android.mjs";

  if (existsSync(configPath)) {
    const existing = readFileSync(configPath, "utf8");
    if (!existing.startsWith(marker)) {
      throw new Error(
        `No se reemplazó ${configPath} porque contiene una configuración Cargo administrada manualmente.`,
      );
    }
  }

  const sections = Object.entries(directories).map(([target, libDir]) => {
    // Never add the NDK's whole unversioned library directory to -L: its
    // libc.a shadows Android's API-specific libc.so and crashes during startup.
    const cppOnly = join(projectRoot, "src-tauri", "target", "android-cpp", target);
    mkdirSync(cppOnly, { recursive: true });
    for (const name of ["libc++_static.a", "libc++abi.a"]) {
      const source = join(libDir, name);
      const destination = join(cppOnly, name);
      if (!existsSync(destination) || statSync(source).size !== statSync(destination).size || !readFileSync(source).equals(readFileSync(destination))) {
        copyFileSync(source, destination);
      }
    }
    const portablePath = cppOnly.replaceAll("\\", "/");
    return `[target.${JSON.stringify(target)}]\nrustflags = ["-L", ${JSON.stringify(`native=${portablePath}`)}]`;
  });

  if (!sections.length) {
    throw new Error("No se encontró libc++_static.a para ninguna ABI Android soportada");
  }

  mkdirSync(cargoDirectory, { recursive: true });
  writeFileSync(
    configPath,
    `${marker}\n# Machine-specific; refreshed before every Android command.\n\n${sections.join("\n\n")}\n`,
    "utf8",
  );
}

function runPnpmTauri(env) {
  return new Promise((resolveResult) => {
    const child = spawn(process.execPath, [join(projectRoot, "node_modules", "@tauri-apps", "cli", "tauri.js"), "android", command, ...forwardedArgs], {
      cwd: projectRoot, env, shell: false, windowsHide: true,
      stdio: command === "build" ? ["inherit", "pipe", "pipe"] : "inherit",
    });
    let stdout = "";
    let stderr = "";
    child.stdout?.on("data", data => { stdout += data; process.stdout.write(data); });
    child.stderr?.on("data", data => { stderr += data; process.stderr.write(data); });
    child.on("error", error => resolveResult({ status: 1, error, stdout, stderr }));
    child.on("close", status => resolveResult({ status, stdout, stderr }));
  });
}

function argumentValue(name) {
  const index = forwardedArgs.indexOf(name);
  return index >= 0 ? forwardedArgs[index + 1] : null;
}

function newestAndroidTdjson(profile, targetTriple) {
  const buildRoot = join(
    projectRoot,
    "src-tauri",
    "target",
    targetTriple,
    profile,
    "build",
  );
  if (!existsSync(buildRoot)) return null;

  const candidates = readdirSync(buildRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && entry.name.startsWith("nuviodrive-v1-"))
    .map((entry) => join(buildRoot, entry.name, "out", "tdlib", "lib", "libtdjson.so"))
    .filter((candidate) => existsSync(candidate))
    .sort((left, right) => statSync(right).mtimeMs - statSync(left).mtimeMs);
  return candidates[0] ?? null;
}

function windowsNoSymlinkFallback(env, tauriResult) {
  if (process.platform !== "win32" || command !== "build") return null;

  const output = `${tauriResult.stdout ?? ""}\n${tauriResult.stderr ?? ""}`;
  const symlinkDenied = /symbolic link/i.test(output)
    && /(not allowed|developer mode|SeCreateSymbolicLinkPrivilege)/i.test(output);
  const staticCppSearchLost = /could not find native static library [`'\"]?c\+\+_static/i.test(output);
  if (!symlinkDenied && !staticCppSearchLost) return null;

  const requestedTarget = argumentValue("--target") ?? "aarch64";
  const targetConfig = {
    aarch64: { triple: "aarch64-linux-android", jni: "arm64-v8a", gradle: "Arm64" },
    armv7: { triple: "armv7-linux-androideabi", jni: "armeabi-v7a", gradle: "Arm" },
    i686: { triple: "i686-linux-android", jni: "x86", gradle: "X86" },
    x86_64: { triple: "x86_64-linux-android", jni: "x86_64", gradle: "X86_64" },
  }[requestedTarget];
  if (!targetConfig) return null;

  const release = !forwardedArgs.includes("--debug") && !forwardedArgs.includes("-d");
  const profile = release ? "release" : "debug";
  const variant = release ? "Release" : "Debug";
  const source = join(
    projectRoot,
    "src-tauri",
    "target",
    targetConfig.triple,
    profile,
    "libnuviodrive_v1_lib.so",
  );

  if (!existsSync(source)) return null;

  const destination = join(
    projectRoot,
    "src-tauri",
    "gen",
    "android",
    "app",
    "src",
    "main",
    "jniLibs",
    targetConfig.jni,
    "libnuviodrive_v1_lib.so",
  );
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(source, destination);

  const tdjsonSource = newestAndroidTdjson(profile, targetConfig.triple);
  if (!tdjsonSource) {
    throw new Error(`No se encontró libtdjson.so para ${requestedTarget} generada por TDLib.`);
  }
  const tdjsonDestination = join(dirname(destination), "libtdjson.so");
  copyFileSync(tdjsonSource, tdjsonDestination);
  console.log(`[Nuvio Android] Incluyendo TDLib nativo: ${tdjsonDestination}`);

  const gradleRoot = join(projectRoot, "src-tauri", "gen", "android");
  const assembleTask = `assemble${targetConfig.gradle}${variant}`;
  const rustTask = `rustBuild${targetConfig.gradle}${variant}`;
  const fallbackReason = staticCppSearchLost
    ? "Gradle perdió la ruta de libc++_static.a"
    : "Windows no permite symlinks";
  console.log(`[Nuvio Android] ${fallbackReason}; reutilizando el binario Rust ${requestedTarget} ya verificado y ejecutando Gradle sin recompilarlo.`);

  return spawnSync(
    "cmd.exe",
    ["/d", "/s", "/c", "gradlew.bat", "clean", assembleTask, "-x", rustTask],
    { cwd: gradleRoot, env, stdio: "inherit", shell: false },
  );
}

function installNuvioMobilePlugin() {
  const source = join(projectRoot, "android", "NuvioMobilePlugin.kt");
  if (!existsSync(source)) throw new Error(`Falta el plugin Android persistente: ${source}`);
  const destination = join(projectRoot, "src-tauri", "gen", "android", "app", "src", "main", "java", "com", "nuvio", "drive", "NuvioMobilePlugin.kt");
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(source, destination);
}

function installAndroidNotificationPermission() {
  const manifest = join(projectRoot, "src-tauri", "gen", "android", "app", "src", "main", "AndroidManifest.xml");
  if (!existsSync(manifest)) return;
  const permission = '<uses-permission android:name="android.permission.POST_NOTIFICATIONS" />';
  const internet = '<uses-permission android:name="android.permission.INTERNET" />';
  const current = readFileSync(manifest, "utf8");
  let next = current;

  // Nuvio is a phone/tablet app. Tauri's generated manifest currently includes
  // optional Android TV/Leanback launcher entries; keeping the launcher category
  // makes Android lint require a TV banner and touchscreen-optional declaration.
  // Remove the TV advertisement instead of pretending the mobile UI supports TV.
  next = next
    .replace(/^[ \t]*<!-- AndroidTV support -->[ \t]*\r?\n/gm, "")
    .replace(/^[ \t]*<uses-feature android:name="android\.software\.leanback"[^>]*\/>[ \t]*\r?\n/gm, "")
    .replace(/^[ \t]*<category android:name="android\.intent\.category\.LEANBACK_LAUNCHER"[ \t]*\/>[ \t]*\r?\n/gm, "");

  if (!next.includes("android.permission.POST_NOTIFICATIONS")) {
    next = next.includes(internet)
      ? next.replace(internet, `${internet}\r\n    ${permission}`)
      : next.replace(/<manifest([^>]*)>/, `<manifest$1>\r\n    ${permission}`);
  }
  if (next !== current) writeFileSync(manifest, next, "utf8");
}

function installAndroidStartupAppearance() {
  const androidRoot = join(projectRoot, "src-tauri", "gen", "android", "app", "src", "main");
  const javaDirectory = join(androidRoot, "java", "com", "nuvio", "drive");
  const drawableDirectory = join(androidRoot, "res", "drawable");
  const valuesDirectory = join(androidRoot, "res", "values");
  const valuesNightDirectory = join(androidRoot, "res", "values-night");
  const valuesV31Directory = join(androidRoot, "res", "values-v31");
  if (!existsSync(javaDirectory) || !existsSync(valuesDirectory)) return;

  mkdirSync(drawableDirectory, { recursive: true });
  mkdirSync(valuesNightDirectory, { recursive: true });
  mkdirSync(valuesV31Directory, { recursive: true });

  // Keep an intentional Nuvio surface behind the WebView while Android starts
  // Chromium/Tauri. On slower emulators this avoids several seconds of a plain
  // white window that looks like a failed launch even though React is healthy.
  writeFileSync(join(drawableDirectory, "nuvio_startup_icon.xml"), `<?xml version="1.0" encoding="utf-8"?>\r\n<vector xmlns:android="http://schemas.android.com/apk/res/android"\r\n    android:width="72dp" android:height="72dp"\r\n    android:viewportWidth="24" android:viewportHeight="24">\r\n    <path\r\n        android:pathData="M17.5,19H9A7,7 0,1 1,15.71,10H17.5A4.5,4.5 0,1 1,17.5,19Z"\r\n        android:fillColor="#00000000" android:strokeColor="#FFFFFFFF"\r\n        android:strokeWidth="1.8" android:strokeLineCap="round" android:strokeLineJoin="round" />\r\n</vector>\r\n`, "utf8");
  writeFileSync(join(drawableDirectory, "nuvio_startup.xml"), `<?xml version="1.0" encoding="utf-8"?>\r\n<layer-list xmlns:android="http://schemas.android.com/apk/res/android">\r\n    <item>\r\n        <shape android:shape="rectangle"><solid android:color="#17191F" /></shape>\r\n    </item>\r\n    <item android:width="72dp" android:height="72dp" android:gravity="center" android:drawable="@drawable/nuvio_startup_icon" />\r\n</layer-list>\r\n`, "utf8");

  const theme = `<?xml version="1.0" encoding="utf-8"?>\r\n<resources>\r\n    <style name="Theme.nuviodrive_v1" parent="Theme.MaterialComponents.DayNight.NoActionBar">\r\n        <item name="android:windowBackground">@drawable/nuvio_startup</item>\r\n        <item name="android:statusBarColor">#17191F</item>\r\n        <item name="android:navigationBarColor">#17191F</item>\r\n        <item name="android:windowLightStatusBar">false</item>\r\n        <item name="android:windowLightNavigationBar">false</item>\r\n    </style>\r\n</resources>\r\n`;
  writeFileSync(join(valuesDirectory, "themes.xml"), theme, "utf8");
  writeFileSync(join(valuesNightDirectory, "themes.xml"), theme, "utf8");
  writeFileSync(join(valuesV31Directory, "themes.xml"), `<?xml version="1.0" encoding="utf-8"?>\r\n<resources>\r\n    <style name="Theme.nuviodrive_v1" parent="Theme.MaterialComponents.DayNight.NoActionBar">\r\n        <item name="android:windowBackground">@drawable/nuvio_startup</item>\r\n        <item name="android:windowSplashScreenBackground">#17191F</item>\r\n        <item name="android:windowSplashScreenAnimatedIcon">@drawable/nuvio_startup_icon</item>\r\n        <item name="android:windowSplashScreenAnimationDuration">200</item>\r\n        <item name="android:statusBarColor">#17191F</item>\r\n        <item name="android:navigationBarColor">#17191F</item>\r\n        <item name="android:windowLightStatusBar">false</item>\r\n        <item name="android:windowLightNavigationBar">false</item>\r\n    </style>\r\n</resources>\r\n`, "utf8");

  writeFileSync(join(javaDirectory, "MainActivity.kt"), `package com.nuvio.drive\r\n\r\nimport android.graphics.Color\r\nimport android.os.Bundle\r\nimport android.view.View\r\nimport android.view.ViewGroup\r\nimport android.webkit.WebView\r\nimport androidx.activity.enableEdgeToEdge\r\n\r\nclass MainActivity : TauriActivity() {\r\n  override fun onCreate(savedInstanceState: Bundle?) {\r\n    enableEdgeToEdge()\r\n    super.onCreate(savedInstanceState)\r\n    window.setBackgroundDrawableResource(R.drawable.nuvio_startup)\r\n    prepareWebViewStartupBackground()\r\n  }\r\n\r\n  private fun prepareWebViewStartupBackground(attempt: Int = 0) {\r\n    val webView = findWebView(window.decorView)\r\n    if (webView != null) {\r\n      webView.setBackgroundColor(Color.TRANSPARENT)\r\n      return\r\n    }\r\n    if (attempt < 40) {\r\n      window.decorView.postDelayed({ prepareWebViewStartupBackground(attempt + 1) }, 50L)\r\n    }\r\n  }\r\n\r\n  private fun findWebView(view: View): WebView? {\r\n    if (view is WebView) return view\r\n    if (view is ViewGroup) {\r\n      for (index in 0 until view.childCount) {\r\n        findWebView(view.getChildAt(index))?.let { return it }\r\n      }\r\n    }\r\n    return null\r\n  }\r\n}\r\n`, "utf8");
}

function modernizeAndroidProject() {
  const androidRoot = join(projectRoot, "src-tauri", "gen", "android");
  const rootBuild = join(androidRoot, "build.gradle.kts");
  const appBuild = join(androidRoot, "app", "build.gradle.kts");
  const buildSrcBuild = join(androidRoot, "buildSrc", "build.gradle.kts");
  const buildTask = join(androidRoot, "buildSrc", "src", "main", "java", "com", "nuvio", "drive", "kotlin", "BuildTask.kt");
  const tauriBuild = join(androidRoot, "app", "tauri.build.gradle.kts");
  const gradleProperties = join(androidRoot, "gradle.properties");
  const wrapper = join(androidRoot, "gradle", "wrapper", "gradle-wrapper.properties");

  if (![rootBuild, appBuild, buildSrcBuild, buildTask, gradleProperties, wrapper].every(existsSync)) return;

  // Keep the Android build toolchain current while targeting the latest stable SDK.
  // Android 17 / API 37 is still a Preview SDK, so production Nuvio remains on API 36.
  // Tauri 2.11.5 and its Android plugins still use the legacy Kotlin Android DSL.
  // AGP 8.11.1 is the newest line fully compatible with Kotlin 2.2.21, which
  // still accepts the kotlinOptions DSL generated by Tauri 2.11.5 and its plugins.
  let root = readFileSync(rootBuild, "utf8")
    .replace(/com\.android\.tools\.build:gradle:[^"\r\n]+/g, "com.android.tools.build:gradle:8.11.1")
    .replace(/org\.jetbrains\.kotlin:kotlin-gradle-plugin:[^"\r\n]+/g, "org.jetbrains.kotlin:kotlin-gradle-plugin:2.2.21");
  if (!root.includes("org.jetbrains.kotlin:kotlin-gradle-plugin:")) {
    root = root.replace(
      /classpath\("com\.android\.tools\.build:gradle:8\.11\.1"\)/,
      'classpath("com.android.tools.build:gradle:8.11.1")\r\n        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.2.21")',
    );
  }
  writeFileSync(rootBuild, root, "utf8");

  let app = readFileSync(appBuild, "utf8")
    .replace(/compileSdk\s*=\s*\d+/g, "compileSdk = 36")
    .replace(/targetSdk\s*=\s*\d+/g, "targetSdk = 36")
    .replace(/androidx\.webkit:webkit:[^"\r\n]+/g, "androidx.webkit:webkit:1.17.0")
    .replace(/androidx\.appcompat:appcompat:[^"\r\n]+/g, "androidx.appcompat:appcompat:1.8.0")
    .replace(/androidx\.activity:activity-ktx:[^"\r\n]+/g, "androidx.activity:activity-ktx:1.13.0")
    .replace(/com\.google\.android\.material:material:[^"\r\n]+/g, "com.google.android.material:material:1.14.0")
    .replace(/androidx\.lifecycle:lifecycle-process:[^"\r\n]+/g, "androidx.lifecycle:lifecycle-process:2.11.0")
    .replace(/androidx\.test\.ext:junit:[^"\r\n]+/g, "androidx.test.ext:junit:1.3.0")
    .replace(/androidx\.test\.espresso:espresso-core:[^"\r\n]+/g, "androidx.test.espresso:espresso-core:3.7.0");

  if (!app.includes('id("org.jetbrains.kotlin.android")')) {
    app = app.replace(
      'id("com.android.application")',
      'id("com.android.application")\r\n    id("org.jetbrains.kotlin.android")',
    );
  }
  if (!app.includes("sourceCompatibility = JavaVersion.VERSION_17")) {
    app = app.replace(
      /\r?\n\s*buildFeatures\s*\{/,
      `\r\n    compileOptions {\r\n        sourceCompatibility = JavaVersion.VERSION_17\r\n        targetCompatibility = JavaVersion.VERSION_17\r\n    }\r\n    buildFeatures {`,
    );
  }
  if (!app.includes('jvmTarget = "17"')) {
    app = app.replace(
      /\r?\n\s*buildFeatures\s*\{/,
      `\r\n    kotlinOptions {\r\n        jvmTarget = "17"\r\n    }\r\n    buildFeatures {`,
    );
  }
  writeFileSync(appBuild, app, "utf8");

  const buildSrc = readFileSync(buildSrcBuild, "utf8")
    .replace(/com\.android\.tools\.build:gradle:[^"\r\n]+/g, "com.android.tools.build:gradle:8.11.1");
  writeFileSync(buildSrcBuild, buildSrc, "utf8");

  // Gradle 9 removed Project.exec. Tauri 2.11.5 still generates BuildTask with
  // that API, so migrate the generated task to the supported injected service.
  let buildTaskSource = readFileSync(buildTask, "utf8");
  if (!buildTaskSource.includes("org.gradle.process.ExecOperations")) {
    buildTaskSource = buildTaskSource.replace(
      "import org.gradle.api.tasks.TaskAction\r\n",
      "import org.gradle.api.tasks.TaskAction\r\nimport org.gradle.process.ExecOperations\r\nimport javax.inject.Inject\r\n",
    );
  }
  buildTaskSource = buildTaskSource
    .replace(
      "open class BuildTask : DefaultTask() {",
      "abstract class BuildTask : DefaultTask() {\r\n    @get:Inject\r\n    abstract val execOperations: ExecOperations",
    )
    .replace("project.exec {", "execOperations.exec {");
  writeFileSync(buildTask, buildTaskSource, "utf8");

  if (existsSync(tauriBuild)) {
    const tauriBuildText = readFileSync(tauriBuild, "utf8")
      .replace(/androidx\.lifecycle:lifecycle-process:[^"\r\n]+/g, "androidx.lifecycle:lifecycle-process:2.11.0")
      .replace("val implementation by configurations", 'val implementation = configurations.getByName("implementation")');
    writeFileSync(tauriBuild, tauriBuildText, "utf8");
  }

  const properties = readFileSync(gradleProperties, "utf8")
    .replace(/^android\.builtInKotlin\s*=.*\r?\n?/gm, "")
    .replace(/^android\.newDsl\s*=.*\r?\n?/gm, "");
  writeFileSync(gradleProperties, properties, "utf8");

  const wrapperText = readFileSync(wrapper, "utf8")
    .replace(/gradle-[0-9.]+-bin\.zip/g, "gradle-8.13-bin.zip");
  writeFileSync(wrapper, wrapperText, "utf8");
}

const env = { ...process.env };
// This override is a Windows DLL SDK, never an Android library.
delete env.NUVIO_TDLIB_DIR;
const sdkRoot = detectSdkRoot();
const ndkRoot = detectNdkRoot();
const java17 = detectJava17();

if (!sdkRoot) {
  console.error("No se encontró Android SDK. Define ANDROID_HOME o ANDROID_SDK_ROOT.");
  process.exit(2);
}
if (!ndkRoot) {
  console.error("No se encontró Android NDK. Instálalo desde Android Studio o define NDK_HOME.");
  process.exit(2);
}
if (!java17) {
  console.error("Nuvio Android requiere JDK 17 para este proyecto. No se encontró una instalación compatible.");
  process.exit(2);
}

env.ANDROID_HOME = sdkRoot;
env.ANDROID_SDK_ROOT = sdkRoot;
env.NDK_HOME = ndkRoot;
env.ANDROID_NDK_HOME = ndkRoot;
env.JAVA_HOME = java17;

const isAndroidBuild = command === "build";
const isDebugBuild = isAndroidBuild && (forwardedArgs.includes("--debug") || forwardedArgs.includes("-d"));
if (isDebugBuild) {
  // Keep phone-test APKs small without changing the normal desktop dev profile.
  env.CARGO_PROFILE_DEV_DEBUG = "0";
  env.CARGO_PROFILE_DEV_STRIP = "symbols";
} else if (isAndroidBuild) {
  // Full LTO + one codegen unit is disproportionately slow with statically linked TDLib.
  // Thin LTO preserves release optimization while allowing useful parallelism.
  env.CARGO_PROFILE_RELEASE_LTO = "thin";
  env.CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "8";
}
writeCargoAndroidConfig(ndkRoot);
if (isAndroidBuild) {
  const requestedTarget = argumentValue("--target") ?? "aarch64";
  const targetTriple = {
    aarch64: "aarch64-linux-android",
    armv7: "armv7-linux-androideabi",
    i686: "i686-linux-android",
    x86_64: "x86_64-linux-android",
  }[requestedTarget];
  if (!targetTriple) throw new Error(`Target Android no soportado: ${requestedTarget}`);
  const cppOnly = join(projectRoot, "src-tauri", "target", "android-cpp", targetTriple);
  if (!existsSync(join(cppOnly, "libc++_static.a"))) {
    throw new Error(`Falta libc++_static.a preparada para ${targetTriple}`);
  }
  const encodedSeparator = "\u001f";
  const inheritedEncoded = env.CARGO_ENCODED_RUSTFLAGS
    ? env.CARGO_ENCODED_RUSTFLAGS.split(encodedSeparator).filter(Boolean)
    : [];
  env.CARGO_ENCODED_RUSTFLAGS = [
    ...inheritedEncoded,
    "-L",
    `native=${cppOnly.replaceAll("\\", "/")}`,
    "-Clink-arg=-landroid",
    "-Clink-arg=-llog",
    "-Clink-arg=-lOpenSLES",
  ].join(encodedSeparator);
}

console.log(`[Nuvio Android] SDK: ${sdkRoot}`);
console.log(`[Nuvio Android] NDK: ${ndkRoot}`);
console.log(`[Nuvio Android] JDK: ${java17}`);

if (command !== "init") {
  installNuvioMobilePlugin();
  installAndroidNotificationPermission();
  installAndroidStartupAppearance();
  modernizeAndroidProject();
}
const releaseBuildLock = acquireBuildLock();
let result = await runPnpmTauri(env);
if (command === "init" && (result.status ?? 1) === 0) {
  installNuvioMobilePlugin();
  installAndroidNotificationPermission();
  installAndroidStartupAppearance();
  modernizeAndroidProject();
}

if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}

if ((result.status ?? 1) !== 0) {
  const fallback = windowsNoSymlinkFallback(env, result);
  if (fallback) result = fallback;
}

if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}
process.exit(result.status ?? 1);
