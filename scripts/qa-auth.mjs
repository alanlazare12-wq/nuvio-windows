import { createRequire } from "node:module";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import assert from "node:assert/strict";

const require = createRequire(import.meta.url);
const packageRoot = process.env.QA_PLAYWRIGHT_PATH || "playwright";
const { chromium } = require(packageRoot);
const { expect } = require(`${packageRoot}/test`);
const browser = await chromium.launch({ channel: process.env.QA_BROWSER_CHANNEL || (process.platform === "win32" ? "msedge" : undefined), headless: true });
const output = path.resolve("qa");
await mkdir(output, { recursive: true });
const results = [];
const url = process.env.QA_BASE_URL || "http://localhost:1420";

async function setup(viewport = { width: 1280, height: 820 }) {
  const context = await browser.newContext({ viewport, hasTouch: viewport.width < 600, isMobile: viewport.width < 600 });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(() => {
    const files = Array.from({ length: 16 }, (_, index) => ({
      id: `file-${index}`, name: index === 15 ? "LEEME" : `Archivo ${String(index).padStart(2, "0")} con nombre largo`,
      extension: index < 2 ? "wav" : index === 15 ? "" : "txt",
      kind: index < 2 ? "audio" : "document", sizeBytes: 1024 * (index + 1),
      updatedAt: new Date(Date.UTC(2026, 8, 9, 0, 0, 16 - index)).toISOString(),
      favorite: index === 2, trashed: false, folder: "Mi unidad", folderId: null, tags: [], provider: "telegram",
    }));
    window.__qa = { files, folders: [], calls: [], media: {}, dashboardError: null, dashboardPolls: 0, statusPolls: 0, syncCursor: 0, historyCursor: 0 };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      convertFileSrc: file => `/qa-media/${file}`,
      invoke: async (cmd, args = {}) => {
        const qa = window.__qa;
        qa.calls.push({ cmd, args });
        if (cmd === "get_dashboard") {
          qa.dashboardPolls++;
          if (qa.dashboardError) throw qa.dashboardError;
          return {
            syncProgress: qa.syncProgress, files: [], folders: structuredClone(qa.folders), transfers: [], transferHistory: [], totalBytes: 20000, fileCount: qa.files.filter(file => !file.trashed).length,
            favoriteCount: qa.files.filter(file => file.favorite && !file.trashed).length, trashCount: qa.files.filter(file => file.trashed).length, recentCount: qa.files.filter(file => !file.trashed).length,
            telegramConnected: true, telegramAccountLabel: "Cuenta QA", providerStatus: "Conectado · entorno de pruebas",
            queueSummary: { total: 0, completed: 0, pending: 0, failed: 0, active: 0, processedBytes: 0, totalBytes: 0, speedBps: 0, cacheBytes: 0, cacheLimitBytes: 2147483648 },
            settings: { preparationConcurrency: 2, uploadConcurrency: 1, downloadConcurrency: 2, cacheLimitBytes: 2147483648, rememberSession: false, conflictPolicy: "skip", deleteOriginalAfterUpload: false },
            catalogCursor: qa.syncCursor,
            historyCursor: qa.historyCursor,
          };
        }
        if (cmd === "get_dashboard_status") {
          qa.statusPolls++;
          if (qa.dashboardError) throw qa.dashboardError;
          return {
            syncProgress: qa.syncProgress, transfers: [], transferHistory: args.afterHistoryCursor === qa.historyCursor ? null : [], totalBytes: 20000, fileCount: qa.files.filter(file => !file.trashed).length,
            favoriteCount: qa.files.filter(file => file.favorite && !file.trashed).length, trashCount: qa.files.filter(file => file.trashed).length, recentCount: qa.files.filter(file => !file.trashed).length,
            telegramConnected: true, telegramAccountLabel: "Cuenta QA", providerStatus: "Conectado · entorno de pruebas",
            queueSummary: { total: 0, completed: 0, pending: 0, failed: 0, active: 0, processedBytes: 0, totalBytes: 0, speedBps: 0, cacheBytes: 0, cacheLimitBytes: 2147483648 },
            settings: { preparationConcurrency: 2, uploadConcurrency: 1, downloadConcurrency: 2, cacheLimitBytes: 2147483648, rememberSession: false, conflictPolicy: "skip", deleteOriginalAfterUpload: false },
            catalogCursor: qa.syncCursor,
            historyCursor: qa.historyCursor,
          };
        }
        if (cmd === "get_catalog_page") {
          const needle = String(args.search ?? "").trim().toLowerCase();
          let rows = qa.files.filter(file => args.section === "trash" ? file.trashed : !file.trashed);
          if (args.section === "favorites") rows = rows.filter(file => file.favorite);
          if (args.kind && args.kind !== "all") rows = rows.filter(file => file.kind === args.kind);
          if (args.tag) rows = rows.filter(file => file.tags?.some(tag => tag.toLowerCase() === String(args.tag).toLowerCase()));
          if (args.section === "files" && !needle) rows = rows.filter(file => (file.folderId ?? null) === (args.folderId ?? null));
          if (needle) rows = rows.filter(file => `${file.name} ${file.extension} ${file.folder ?? ""} ${(file.tags ?? []).join(" ")}`.toLowerCase().includes(needle));
          rows = [...rows].sort((a, b) => {
            if (args.sort === "name") return a.name.localeCompare(b.name);
            if (args.sort === "size") return b.sizeBytes - a.sizeBytes;
            const diff = (Date.parse(a.updatedAt) || 0) - (Date.parse(b.updatedAt) || 0);
            return args.sort === "oldest" ? diff : -diff;
          });
          const total = rows.length;
          const offset = Math.max(0, Number(args.offset ?? 0));
          const limit = Math.max(1, Number(args.limit ?? 80));
          const files = rows.slice(offset, offset + limit);
          return { files: structuredClone(files), total, offset, limit, hasMore: offset + files.length < total };
        }
        if (cmd === "update_setting") return {};
        if (cmd === "set_trashed") { qa.files.find(file => file.id === args.id).trashed = args.trashed; qa.syncCursor++; return; }
        if (cmd === "platform_name") return "windows";
        if (cmd === "create_folder") {
          const id = `folder-${qa.folders.length}`;
          qa.folders.push({id, name: args.name, parentId: args.parentId, trashed: false, fileCount: 0, childCount: 0, sizeBytes: 0, createdAt: 0, updatedAt: 0});
          qa.syncCursor++;
          return id;
        }
        if (cmd === "move_files_to_folder") {
          qa.files.filter(file => args.ids.includes(file.id)).forEach(file => { file.folderId = args.folderId; });
          qa.syncCursor++;
          return args.ids.length;
        }
        if (cmd === "set_favorite") { qa.files.find(file => file.id === args.id).favorite = args.favorite; qa.syncCursor++; return; }
        if (cmd === "prepare_media") return new Promise(resolve => { qa.media[args.id] = resolve; });
        if (cmd === "prepare_thumbnail_batch") {
          return (args.ids ?? []).map(id => ({ id, source: qa.thumbnails?.[id] ?? null }));
        }
        if (cmd === "prepare_thumbnail") return qa.thumbnails?.[args.id] ?? null;
        if (cmd === "telegram_auth_state") return qa.telegramAuthState ?? { stage: "needsCredentials", connected: false, message: "Configura tus credenciales", isPremium: false };
        if (cmd === "telegram_configure") {
          qa.telegramAuthState = { stage: "phone", connected: false, message: "Ingresa el teléfono", isPremium: false };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_submit_phone") {
          qa.telegramAuthState = { stage: "code", connected: false, message: "Ingresa el código", hint: "Código en app oficial de Telegram", isPremium: false, timeout: 60, codeType: "telegram", nextCodeType: "sms" };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_submit_phone_sms") {
          qa.telegramAuthState = { stage: "code", connected: false, message: "Ingresa el código", hint: "Código en app oficial de Telegram", isPremium: false, timeout: 60, codeType: "telegram", nextCodeType: "sms" };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_resend_code") {
          qa.telegramAuthState = { stage: "code", connected: false, message: "Ingresa el código", hint: "Código por SMS a tu celular", isPremium: false, timeout: 60, codeType: "sms", nextCodeType: "call" };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_submit_email") {
          qa.telegramAuthState = { stage: "emailCode", connected: false, message: "Ingresa el código enviado al correo", hint: "Revisa tu correo", isPremium: false, allowGoogleId: false, allowAppleId: false, futureAuthTokenCount: 0 };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_submit_email_identity") {
          qa.telegramAuthState = { stage: "password", connected: false, message: "Verificación en dos pasos", hint: "Contraseña adicional", isPremium: false, allowGoogleId: false, allowAppleId: false, futureAuthTokenCount: 0 };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_reset_authentication_email") {
          qa.telegramAuthState = { stage: "phone", connected: false, message: "Ingresa el teléfono", isPremium: false, allowGoogleId: false, allowAppleId: false, futureAuthTokenCount: 0 };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_login_email_status") {
          return qa.loginEmailStatus ?? { available: false, required: false, emailPattern: null };
        }
        if (cmd === "telegram_set_login_email" || cmd === "telegram_resend_login_email") {
          return { emailPattern: "q***@example.com", codeLength: 6 };
        }
        if (cmd === "telegram_check_login_email") {
          qa.loginEmailStatus = { available: true, required: false, emailPattern: "q***@example.com" };
          qa.telegramAuthState = { stage: "ready", connected: true, message: "Telegram conectado", accountLabel: "Cuenta QA", isPremium: false, allowGoogleId: false, allowAppleId: false, futureAuthTokenCount: 0 };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_request_qr") {
          qa.telegramAuthState = { stage: "qr", connected: false, message: "Escanea desde Telegram", qrSvg: '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200"><rect width="200" height="200" fill="black"/></svg>', isPremium: false };
          return qa.telegramAuthState;
        }
        if (cmd === "telegram_reset_to_phone") {
          qa.telegramAuthState = { stage: "phone", connected: false, message: "Ingresa el teléfono", isPremium: false };
          return qa.telegramAuthState;
        }
        if (cmd === "plugin:dialog|open") return qa.uploadPaths ?? null;
        if (cmd === "plugin:opener|open_url") return;
        if (cmd === "plugin:event|listen") return 1;
        if (cmd === "plugin:event|unlisten") return;
        if (cmd === "prepare_zip_uploads") {
          if (qa.zipError) throw qa.zipError;
          return [{ Ok: { transferId: "zip-qa", fileName: "Nuvio-001.zip", localPath: "zip-qa.zip", sizeBytes: 100, sha256: "qa", duplicate: false, encrypted: false, status: "ready" } }];
        }
        if (cmd === "sync_files") {
          return qa.syncResultCount ?? 0;
        }
        if (cmd.startsWith("plugin:notification|")) return false;
        throw new Error(`Unexpected IPC call in QA: ${cmd}`);
      },
    };
  });
  await page.route("**/qa-media/**", route => route.fulfill({ status: 200, contentType: "audio/wav", body: Buffer.alloc(44) }));
  await page.goto(url);
  await expect(page.getByRole("heading", { name: "Inicio", exact: true })).toBeVisible();
  return { context, page, errors };
}

async function test(name, fn, viewport) {
  if (process.env.QA_FILTER && !name.includes(process.env.QA_FILTER)) return;
  const { context, page, errors } = await setup(viewport);
  try {
    await fn(page);
    assert.deepEqual(errors, [], "Browser raised an uncaught exception");
    results.push({ name, status: "passed" });
    console.log(`PASS ${name}`);
  } catch (error) {
    results.push({ name, status: "failed", error: error.message });
    console.error(`FAIL ${name}: ${error.message}`);
    await page.screenshot({ path: path.join(output, `${name}-failure.png`), fullPage: true });
  } finally { await context.close(); }
}

try {
  for (const [kind, label, next] of [["telegram", "mensaje de Telegram", "sms"], ["sms", "SMS al teléfono", "call"], ["call", "llamada telefónica", "telegram"]]) {
    await test(`auth-channel-${kind}`, async page => {
      await page.evaluate(({kind,next}) => { window.__qa.telegramAuthState = { stage:"code", connected:false, message:"Verificación", codeType:kind, nextCodeType:next, timeout:2 }; }, {kind,next});
      await page.locator(".profile-button").click();
      await expect(page.getByLabel(`Código por ${label}`, {exact:true})).toBeVisible();
      await expect(page.getByRole("button", {name:/Esperar .*s para reenviar/})).toBeDisabled();
      await page.clock.install();
      await page.clock.fastForward(65000);
      await expect(page.getByRole("button", {name:/Solicitar código por/})).toBeEnabled();
      await page.getByRole("button", {name:/Solicitar código por/}).click();
      assert.equal(await page.evaluate(() => window.__qa.calls.filter(c=>c.cmd==="telegram_resend_code").length),1);
    });
  }
  await test("auth-email-resend-uses-correct-command", async page => {
    await page.evaluate(() => {
      window.__qa.telegramAuthState={stage:"email",connected:false,message:"Correo requerido"};
      const original=window.__TAURI_INTERNALS__.invoke;
      window.__TAURI_INTERNALS__.invoke=async(cmd,args)=>{
        if(cmd==="telegram_resend_code") { window.__qa.calls.push({cmd,args}); return {stage:"emailCode",connected:false,message:"Código de correo",hint:"a***@example.com"}; }
        return original(cmd,args);
      };
    });
    await page.locator(".profile-button").click();
    await page.getByLabel("Correo de autenticación solicitado por Telegram").fill("qa@example.com");
    await page.getByRole("button",{name:"Enviar código al correo",exact:true}).click();
    await expect(page.getByLabel("Código de correo electrónico",{exact:true})).toBeVisible();
    await page.getByRole("button",{name:"Reenviar código al correo",exact:true}).click();
    const commands=await page.evaluate(()=>window.__qa.calls.filter(c=>c.cmd==="telegram_submit_email"||c.cmd==="telegram_resend_code").map(c=>c.cmd));
    assert.deepEqual(commands,["telegram_submit_email","telegram_resend_code"]);
  });
  await test("auth-no-false-channel-and-preserve-number-on-error",async page=>{
    await page.evaluate(()=>{
      window.__qa.telegramAuthState={stage:"code",connected:false,message:"Verificación",codeType:"telegram",nextCodeType:null,timeout:0};
      const original=window.__TAURI_INTERNALS__.invoke;
      window.__TAURI_INTERNALS__.invoke=async(cmd,args)=>{if(cmd==="telegram_submit_phone")throw "Número rechazado por Telegram";return original(cmd,args);};
    });
    await page.locator(".profile-button").click();
    await expect(page.getByText("Telegram no ofrece un canal de reenvío en este momento.")).toBeVisible();
    await expect(page.getByRole("button",{name:/Solicitar código por/})).toHaveCount(0);
    await page.getByRole("button",{name:"Corregir número de teléfono",exact:true}).click();
    await page.getByLabel("Teléfono con código de país").fill("+525555555555");
    await page.getByRole("button",{name:"Continuar con mi número",exact:true}).click();
    await expect(page.getByRole("alert")).toContainText("Número rechazado");
    await expect(page.getByLabel("Teléfono con código de país")).toHaveValue("+525555555555");
    assert.equal(await page.evaluate(()=>window.__qa.calls.some(c=>c.cmd==="telegram_reset_to_phone")),false);
  });
  await test("auth-phone-email-gating-and-qr-return",async page=>{
    await page.evaluate(()=>{window.__qa.telegramAuthState={stage:"phone",connected:false,message:"Teléfono"};});
    await page.locator(".profile-button").click();
    await expect(page.locator(".auth-delivery-option")).toHaveCount(5);
    await expect(page.locator("#telegram-email")).toHaveCount(0);
    await page.getByRole("button",{name:"Vincular con código QR",exact:true}).click();
    await expect(page.locator(".telegram-qr svg")).toBeVisible();
    await page.getByRole("button",{name:"Volver al teléfono",exact:true}).click();
    await expect(page.getByLabel("Teléfono con código de país")).toBeVisible();
  },{width:412,height:915});

  for (const kind of ["telegram", "sms", "call", "email"]) {
    await test(`auth-submit-${kind}-reject-retry-and-2fa`, async page => {
      await page.evaluate(kind => {
        window.__qa.telegramAuthState={stage:kind==="email"?"emailCode":"code",connected:false,message:"Introduce el código",codeType:kind,nextCodeType:null};
        const original=window.__TAURI_INTERNALS__.invoke;
        window.__TAURI_INTERNALS__.invoke=async(cmd,args)=>{
          if(cmd==="telegram_submit_code"||cmd==="telegram_submit_email_code"){
            window.__qa.calls.push({cmd,args});
            await new Promise(resolve=>setTimeout(resolve,150));
            if(args.code!=="12345")throw "El código es incorrecto. Vuelve a intentarlo.";
            return {stage:"password",connected:false,message:"Verificación en dos pasos",hint:"Contraseña adicional"};
          }
          return original(cmd,args);
        };
      },kind);
      await page.locator(".profile-button").click();
      await page.locator("#telegram-code").fill("00000");
      await page.getByRole("button",{name:"Continuar",exact:true}).click();
      await expect(page.getByRole("alert")).toContainText("código es incorrecto");
      await expect(page.locator("#telegram-code")).toHaveValue("00000");
      await page.locator("#telegram-code").fill("12345");
      await page.getByRole("button",{name:"Continuar",exact:true}).click();
      await expect(page.getByLabel("Contraseña de verificación en dos pasos")).toBeVisible();
      const calls=await page.evaluate(()=>window.__qa.calls.filter(c=>c.cmd==="telegram_submit_code"||c.cmd==="telegram_submit_email_code"));
      assert.deepEqual(calls.map(c=>c.cmd),Array(2).fill(kind==="email"?"telegram_submit_email_code":"telegram_submit_code"));
      assert.deepEqual(calls.map(c=>c.args.code),["00000","12345"]);
    });
  }

  for (const [kind, title] of [["sms", "SMS al teléfono"], ["email", "Correo electrónico"], ["call", "Llamada telefónica"], ["telegram", "Mensaje de Telegram"], ["fragment", "Fragment"]]) {
    await test(`auth-choice-${kind}-reaches-backend`, async page => {
      await page.evaluate(kind => {
        window.__qa.telegramAuthState = { stage: "phone", connected: false, message: "Ingresa tu teléfono" };
        const original = window.__TAURI_INTERNALS__.invoke;
        window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
          if (cmd === "telegram_submit_phone") {
            window.__qa.calls.push({ cmd, args });
            window.__qa.telegramAuthState = { stage: kind === "email" ? "email" : "code", codeType: kind, nextCodeType: null, connected: false, message: "Verificación" };
            return window.__qa.telegramAuthState;
          }
          return original(cmd, args);
        };
      }, kind);
      await page.locator(".profile-button").click();
      await expect(page.locator(".auth-delivery-option")).toHaveCount(5);
      await page.getByRole("button", { name: title, exact: true }).click();
      await expect(page.getByRole("button", { name: title, exact: true })).toHaveAttribute("aria-pressed", "true");
      await page.getByLabel("Teléfono con código de país").fill("+525555555555");
      await page.getByRole("button", { name: "Continuar con mi número", exact: true }).click();
      await expect(page.locator(kind === "email" ? "#telegram-email" : "#telegram-code")).toBeVisible();
      const calls = await page.evaluate(() => window.__qa.calls.filter(c => c.cmd === "telegram_submit_phone"));
      assert.deepEqual(calls.map(c => c.args), [{ phone: "+525555555555", delivery: kind }]);
      await expect(page.getByRole("button", { name: title, exact: true })).toHaveAttribute("aria-pressed", "true");
      if (kind === "email") {
        await page.getByLabel("Correo de autenticación solicitado por Telegram").fill("qa@example.com");
        await page.getByRole("button", { name: "Enviar código al correo", exact: true }).click();
        await expect(page.getByLabel("Código de correo electrónico", { exact: true })).toBeVisible();
        assert.deepEqual(await page.evaluate(() => window.__qa.calls.filter(c => c.cmd === "telegram_submit_email").map(c => c.args)), [{ email: "qa@example.com" }]);
      }
    }, { width: 412, height: 915 });
  }

  await test("auth-sms-preference-real-resend-after-telegram", async page => {
    await page.evaluate(() => { window.__qa.telegramAuthState = { stage: "phone", connected: false, message: "Ingresa tu teléfono" }; });
    await page.locator(".profile-button").click();
    await page.getByRole("button", { name: "SMS al teléfono", exact: true }).click();
    await page.getByLabel("Teléfono con código de país").fill("+525555555555");
    await page.getByRole("button", { name: "Continuar con mi número", exact: true }).click();
    await expect(page.getByLabel("Código por mensaje de Telegram", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "SMS al teléfono", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByRole("button", { name: /Esperar .*s para reenviar/ })).toBeDisabled();
    await page.clock.install();
    await page.clock.fastForward(65000);
    await page.getByRole("button", { name: "Solicitar código por SMS al teléfono", exact: true }).click();
    await expect(page.getByLabel("Código por SMS al teléfono", { exact: true })).toBeVisible();
    assert.deepEqual(await page.evaluate(() => window.__qa.calls.filter(c => c.cmd === "telegram_resend_code").map(c => c.args)), [{ delivery: "sms" }]);
    await expect(page.locator(".auth-delivery-explanation")).toContainText("Revisa la app de mensajes");
    await page.screenshot({ path: path.join(output, "auth-sms-delivered-s24.png"), fullPage: false });
  }, { width: 412, height: 915 });

  await test("auth-unavailable-choice-explains-without-sending", async page => {
    await page.evaluate(() => { window.__qa.telegramAuthState = { stage: "code", connected: false, message: "Verificación", codeType: "telegram", nextCodeType: null }; });
    await page.locator(".profile-button").click();
    for (const title of ["Correo electrónico", "Llamada telefónica", "SMS al teléfono", "Fragment"]) {
      await page.getByRole("button", { name: title, exact: true }).click();
      await expect(page.getByRole("button", { name: title, exact: true })).toHaveAttribute("aria-pressed", "true");
      await expect(page.locator(".auth-delivery-explanation")).toContainText(/no habilitó|no ofrece/);
    }
    assert.equal(await page.evaluate(() => window.__qa.calls.some(c => c.cmd === "telegram_resend_code" || c.cmd === "telegram_submit_email")), false);
    await expect(page.getByRole("button", { name: /Solicitar código por/ })).toHaveCount(0);
  });

  await test("auth-fragment-opens-server-url", async page => {
    await page.evaluate(() => {
      window.__qa.telegramAuthState = {
        stage: "code",
        connected: false,
        message: "Código mediante Fragment",
        codeType: "fragment",
        nextCodeType: null,
        timeout: 0,
        fragmentUrl: "https://fragment.com/number/123",
        codeLength: 5,
        allowGoogleId: false,
        allowAppleId: false,
        futureAuthTokenCount: 0,
      };
    });
    await page.locator(".profile-button").click();
    await expect(page.getByRole("button", { name: "Fragment", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByTestId("fragment-auth")).toBeVisible();
    await expect(page.locator("#telegram-code")).toHaveAttribute("maxlength", "5");
    await page.getByRole("button", { name: "Abrir Fragment", exact: true }).click();
    const calls = await page.evaluate(() => window.__qa.calls.filter(c => c.cmd === "plugin:opener|open_url"));
    assert.equal(calls.length, 1);
    assert.equal(calls[0].args.url, "https://fragment.com/number/123");
  });

  await test("auth-fragment-rejects-untrusted-url", async page => {
    await page.evaluate(() => {
      window.__qa.telegramAuthState = {
        stage: "code",
        connected: false,
        message: "Código mediante Fragment",
        codeType: "fragment",
        nextCodeType: null,
        timeout: 0,
        fragmentUrl: "javascript:alert(1)",
        codeLength: 5,
        allowGoogleId: false,
        allowAppleId: false,
        futureAuthTokenCount: 0,
      };
    });
    await page.locator(".profile-button").click();
    await page.getByRole("button", { name: "Abrir Fragment", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("URL HTTPS válida de Fragment");
    assert.equal(
      await page.evaluate(() => window.__qa.calls.filter(c => c.cmd === "plugin:opener|open_url").length),
      0,
    );
  });

  for (const provider of ["google", "apple"]) {
    await test(`auth-${provider}-identity-token-reaches-tdlib-command`, async page => {
      await page.evaluate(() => {
        window.__qa.telegramAuthState = {
          stage: "emailCode",
          connected: false,
          message: "Verifica el correo",
          hint: "q***@example.com",
          emailPattern: "q***@example.com",
          emailCodeLength: 6,
          allowGoogleId: true,
          allowAppleId: true,
          futureAuthTokenCount: 0,
        };
      });
      await page.locator(".profile-button").click();
      await expect(page.getByTestId("identity-options")).toBeVisible();
      const label = provider === "google" ? "Google ID" : "Apple ID";
      await page.getByRole("button", { name: label, exact: true }).click();
      await page.getByLabel(`ID token de ${provider === "google" ? "Google" : "Apple"}`).fill(`${provider}-qa-token`);
      await page.getByRole("button", { name: `Continuar con ${label}`, exact: true }).click();
      await expect(page.getByLabel("Contraseña de verificación en dos pasos")).toBeVisible();
      const calls = await page.evaluate(() => window.__qa.calls.filter(c => c.cmd === "telegram_submit_email_identity"));
      assert.deepEqual(calls.map(c => c.args), [{ provider, token: `${provider}-qa-token` }]);
    });
  }

  await test("auth-email-reset-returns-to-phone", async page => {
    await page.evaluate(() => {
      window.__qa.telegramAuthState = {
        stage: "emailCode",
        connected: false,
        message: "Verifica el correo",
        emailPattern: "q***@example.com",
        emailCodeLength: 6,
        allowGoogleId: false,
        allowAppleId: false,
        emailReset: { state: "available", seconds: 0 },
        futureAuthTokenCount: 0,
      };
    });
    await page.locator(".profile-button").click();
    await expect(page.getByTestId("email-reset")).toBeVisible();
    await page.getByRole("button", { name: "Restablecer correo de autenticación", exact: true }).click();
    await expect(page.getByLabel("Teléfono con código de país")).toBeVisible();
    assert.equal(await page.evaluate(() => window.__qa.calls.filter(c => c.cmd === "telegram_reset_authentication_email").length), 1);
  });

  await test("auth-future-token-and-passkey-policy-are-explicit", async page => {
    await page.evaluate(() => {
      window.__qa.telegramAuthState = {
        stage: "phone",
        connected: false,
        message: "Ingresa tu teléfono",
        allowGoogleId: false,
        allowAppleId: false,
        futureAuthTokenCount: 2,
      };
    });
    await page.locator(".profile-button").click();
    await expect(page.getByTestId("future-auth-ready")).toContainText("2 tokens");
    await expect(page.getByTestId("passkey-unavailable")).toContainText("No disponible en clientes no oficiales");
    await expect(page.getByTestId("identity-options")).toHaveCount(0);
  });

  await test("auth-connected-can-configure-login-email", async page => {
    await page.evaluate(() => {
      window.__qa.loginEmailStatus = { available: true, required: true, emailPattern: null };
      window.__qa.telegramAuthState = {
        stage: "ready",
        connected: true,
        message: "Telegram conectado",
        accountLabel: "Cuenta QA",
        isPremium: false,
        allowGoogleId: false,
        allowAppleId: false,
        futureAuthTokenCount: 1,
      };
    });
    await page.locator(".profile-button").click();
    await expect(page.getByTestId("login-email-setup")).toBeVisible();
    await expect(page.getByTestId("login-email-required")).toContainText("Telegram solicita configurar");
    await page.getByLabel("Correo para futuros accesos").fill("qa@example.com");
    await page.getByRole("button", { name: "Configurar correo de acceso", exact: true }).click();
    await expect(page.getByLabel("Código del correo de login")).toBeVisible();
    await expect(page.getByLabel("Código del correo de login")).toHaveAttribute("maxlength", "6");
    await page.getByLabel("Código del correo de login").fill("123456");
    await page.getByRole("button", { name: "Verificar correo", exact: true }).click();
    await expect(page.getByTestId("login-email-current")).toContainText("q***@example.com");
    const commands = await page.evaluate(() => window.__qa.calls
      .filter(c => ["telegram_set_login_email", "telegram_check_login_email"].includes(c.cmd))
      .map(c => ({ cmd: c.cmd, args: c.args })));
    assert.deepEqual(commands, [
      { cmd: "telegram_set_login_email", args: { email: "qa@example.com" } },
      { cmd: "telegram_check_login_email", args: { code: "123456" } },
    ]);
  });

  await test("auth-connected-does-not-offer-login-email-when-telegram-disables-it", async page => {
    await page.evaluate(() => {
      window.__qa.loginEmailStatus = { available: false, required: false, emailPattern: null };
      window.__qa.telegramAuthState = {
        stage: "ready",
        connected: true,
        message: "Telegram conectado",
        accountLabel: "Cuenta QA",
        isPremium: false,
        allowGoogleId: false,
        allowAppleId: false,
        futureAuthTokenCount: 0,
      };
    });
    await page.locator(".profile-button").click();
    await expect(page.getByTestId("login-email-unavailable")).toContainText("no habilitó");
    await expect(page.getByLabel("Correo para futuros accesos")).toHaveCount(0);
    assert.equal(
      await page.evaluate(() => window.__qa.calls.filter(c => c.cmd === "telegram_set_login_email").length),
      0,
    );
  });

  for (const [device, viewport] of [["desktop", { width: 1280, height: 820 }], ["s24", { width: 412, height: 915 }], ["s24-keyboard", { width: 412, height: 500 }], ["s24-landscape", { width: 915, height: 412 }]]) {
    await test(`auth-choice-layout-${device}`, async page => {
      await page.evaluate(() => { window.__qa.telegramAuthState = { stage: "phone", connected: false, message: "Ingresa el teléfono asociado a tu cuenta" }; });
      await page.locator(".profile-button").click();
      const dialog = page.getByRole("dialog");
      for (const title of ["SMS al teléfono", "Correo electrónico", "Llamada telefónica", "Mensaje de Telegram", "Fragment"]) {
        const button = page.getByRole("button", { name: title, exact: true });
        await button.click();
        await expect(button).toHaveAttribute("aria-pressed", "true");
        const box = await button.boundingBox();
        assert.ok(box.width >= 44 && box.height >= 44, "Choice must remain a touch target");
      }
      await page.getByRole("button", { name: "SMS al teléfono", exact: true }).click();
      const clipped = await dialog.evaluate(el => el.scrollWidth > el.clientWidth + 1);
      assert.equal(clipped, false, "Dialog must not overflow horizontally");
      await page.getByLabel("Teléfono con código de país").fill("+525555555555");
      await page.getByRole("button", { name: "Continuar con mi número", exact: true }).scrollIntoViewIfNeeded();
      await expect(page.getByRole("button", { name: "Continuar con mi número", exact: true })).toBeInViewport();
      await dialog.evaluate(el => { el.scrollTop = 0; });
      await page.locator("#telegram-phone").blur();
      await page.screenshot({ path: path.join(output, `auth-choices-${device}.png`), fullPage: false });
    }, viewport);
  }

} finally { await browser.close(); await writeFile(path.join(output,"auth-ui-results.json"),JSON.stringify(results,null,2)); }
if(results.some(r=>r.status==="failed"))process.exitCode=1;
