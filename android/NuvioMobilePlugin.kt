package com.nuvio.drive

import android.Manifest
import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ContentResolver
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.security.KeyStore
import java.security.MessageDigest
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties

@InvokeArg
class SecretArgs {
  lateinit var data: String
  var encrypt: Boolean = true
}

@InvokeArg
class UriArgs {
  lateinit var uri: String
}

@InvokeArg
class VerifiedDeleteArgs {
  lateinit var uri: String
  lateinit var sha256: String
  var size: Long = 0
}

@InvokeArg
class WriteArgs {
  lateinit var uri: String
  lateinit var data: String
}

@InvokeArg
class PublishArgs {
  lateinit var source: String
  lateinit var uri: String
  lateinit var name: String
  lateinit var policy: String
  lateinit var sha256: String
  var size: Long = 0
}

@InvokeArg
class NotificationArgs {
  var active: Boolean = false
  var percent: Int? = null
  var scanned: Long? = null
  var total: Long? = null
  var completed: Long? = null
  var pending: Long? = null
  var failed: Long? = null
  var currentFileName: String? = null
  var phase: String? = null
  var error: String? = null
}

@InvokeArg
class NotificationIdArgs {
  var id: Int = 0
}

@TauriPlugin
class NuvioMobilePlugin(private val activity: Activity) : Plugin(activity) {
  companion object {
    private const val KEY_ALIAS = "com.nuvio.drive.secrets.v1"
    private const val CHANNEL_ID = "nuvio_transfers"
    private const val SYNC_NOTIFICATION_ID = 4101
    private const val UPLOAD_NOTIFICATION_ID = 4102
    private const val NOTIFICATION_PERMISSION_REQUEST = 4103
    private const val MAX_TREE_FILES = 10_000
    private const val MAX_TREE_FOLDERS = 10_000
    private const val MAX_TREE_DEPTH = 128
  }

  private var notificationPermissionRequested = false

  @Command
  fun protectSecret(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(SecretArgs::class.java)
      val input = hexDecode(args.data)
      val output = if (args.encrypt) encryptSecret(input) else decryptSecret(input)
      invoke.resolve(JSObject().apply { put("data", hexEncode(output)) })
    } catch (error: Exception) {
      invoke.reject(error.message ?: "No se pudo proteger el secreto")
    }
  }

  @Command
  fun pickDirectory(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
      addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
    }
    startActivityForResult(invoke, intent, "directoryPicked")
  }

  @ActivityCallback
  fun directoryPicked(invoke: Invoke, result: ActivityResult) {
    val uri = result.data?.data
    if (result.resultCode != Activity.RESULT_OK || uri == null) {
      invoke.resolve(JSObject().apply { put("uri", null) })
      return
    }
    persistPermissions(uri, result.data?.flags ?: 0)
    invoke.resolve(JSObject().apply { put("uri", uri.toString()) })
  }

  @Command
  fun directoryNames(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(UriArgs::class.java)
      val names = listChildren(Uri.parse(args.uri)).map { it.name }
      invoke.resolve(JSObject().apply { put("names", JSArray.from(names.toTypedArray())) })
    } catch (error: Exception) {
      invoke.reject(error.message ?: "No se pudo leer la carpeta")
    }
  }

  @Command
  fun pickUploadFiles(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
      addCategory(Intent.CATEGORY_OPENABLE)
      type = "*/*"
      putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
      addFlags(
        Intent.FLAG_GRANT_READ_URI_PERMISSION or
          Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
          Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
      )
    }
    startActivityForResult(invoke, intent, "uploadFilesPicked")
  }

  @ActivityCallback
  fun uploadFilesPicked(invoke: Invoke, result: ActivityResult) {
    if (result.resultCode != Activity.RESULT_OK) {
      invoke.resolve(JSObject().apply { put("paths", JSArray()) })
      return
    }
    val data = result.data
    val paths = mutableListOf<String>()
    if (data?.clipData != null) {
      for (index in 0 until data.clipData!!.itemCount) {
        val uri = data.clipData!!.getItemAt(index).uri
        persistPermissions(uri, data.flags)
        paths.add(uri.toString())
      }
    } else {
      data?.data?.let { uri ->
        persistPermissions(uri, data.flags)
        paths.add(uri.toString())
      }
    }
    invoke.resolve(JSObject().apply { put("paths", JSArray.from(paths.toTypedArray())) })
  }

  @Command
  fun stageContentUri(invoke: Invoke) {
    var directory: File? = null
    try {
      val args = invoke.parseArgs(UriArgs::class.java)
      val uri = Uri.parse(args.uri)
      val name = safeName(displayName(uri) ?: "archivo")
      directory = File(activity.cacheDir, "upload_staging/${UUID.randomUUID()}")
      if (!directory.mkdirs() && !directory.isDirectory) throw IllegalStateException("No se pudo crear la caché privada de Nuvio")
      val destination = File(directory, name)
      val digest = MessageDigest.getInstance("SHA-256")
      var size = 0L
      activity.contentResolver.openInputStream(uri).use { input ->
        if (input == null) throw IllegalStateException("Android no pudo abrir el archivo seleccionado")
        FileOutputStream(destination).use { output ->
          val buffer = ByteArray(1024 * 1024)
          while (true) {
            val read = input.read(buffer)
            if (read < 0) break
            if (read == 0) continue
            output.write(buffer, 0, read)
            digest.update(buffer, 0, read)
            size += read
          }
          output.flush()
          output.fd.sync()
        }
      }
      if (size <= 0L || destination.length() != size) {
        throw IllegalStateException("El archivo seleccionado está vacío o no pudo copiarse completo")
      }
      invoke.resolve(JSObject().apply {
        put("path", destination.absolutePath)
        put("sizeBytes", size)
        put("sha256", digest.digest().joinToString("") { "%02x".format(it) })
      })
    } catch (error: Exception) {
      directory?.deleteRecursively()
      invoke.reject(error.message ?: "No se pudo preparar el archivo seleccionado")
    }
  }

  @Command
  fun pickUploadDirectory(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
      addFlags(
        Intent.FLAG_GRANT_READ_URI_PERMISSION or
          Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
          Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
      )
    }
    startActivityForResult(invoke, intent, "uploadDirectoryPicked")
  }

  @ActivityCallback
  fun uploadDirectoryPicked(invoke: Invoke, result: ActivityResult) {
    val tree = result.data?.data
    if (result.resultCode != Activity.RESULT_OK || tree == null) {
      invoke.resolve(JSObject().apply { put("cancelled", true) })
      return
    }
    try {
      persistPermissions(tree, result.data?.flags ?: 0)
      val rootId = DocumentsContract.getTreeDocumentId(tree)
      val rootUri = DocumentsContract.buildDocumentUriUsingTree(tree, rootId)
      val rootName = documentInfo(rootUri)?.name ?: "Carpeta"
      val folders = mutableListOf<String>()
      val files = JSArray()
      var totalBytes = 0L
      val counters = ScanCounters()
      walkTree(tree, rootId, "", 0, counters, folders, files) { totalBytes += it }
      invoke.resolve(JSObject().apply {
        put("cancelled", false)
        put("rootName", rootName)
        put("folders", JSArray.from(folders.toTypedArray()))
        put("files", files)
        put("totalBytes", totalBytes)
      })
    } catch (error: Exception) {
      invoke.reject(error.message ?: "Error al procesar la carpeta seleccionada")
    }
  }

  @Command
  fun publishDownload(invoke: Invoke) {
    var target: Uri? = null
    try {
      val args = invoke.parseArgs(PublishArgs::class.java)
      val source = File(args.source)
      if (!source.isFile) throw IllegalStateException("El archivo debe estar en la caché privada de Nuvio")
      if (source.length() != args.size) throw IllegalStateException("La descarga local no coincide con Telegram")
      val tree = Uri.parse(args.uri)
      val createdTarget = createDocument(tree, args.name, args.policy)
      target = createdTarget

      var copied = 0L
      activity.contentResolver.openOutputStream(createdTarget, "w").use { output ->
        if (output == null) throw IllegalStateException("Android no concedió permiso para guardar el archivo")
        FileInputStream(source).use { input ->
          val buffer = ByteArray(1024 * 1024)
          while (true) {
            val read = input.read(buffer)
            if (read < 0) break
            if (read == 0) continue
            output.write(buffer, 0, read)
            copied += read
          }
          output.flush()
        }
      }
      if (copied != args.size) {
        runCatching { DocumentsContract.deleteDocument(activity.contentResolver, createdTarget) }
        target = null
        throw IllegalStateException("La descarga local no coincide con Telegram")
      }

      // Verify the bytes that Android actually persisted. Hashing the source while
      // copying is redundant: Rust/TDLib already validated its expected size, and
      // this destination hash is the integrity check that matters for SAF providers.
      val verified = hashContentUri(createdTarget)
      if (verified.first != args.size || verified.second != args.sha256) {
        runCatching { DocumentsContract.deleteDocument(activity.contentResolver, createdTarget) }
        target = null
        throw IllegalStateException("No se pudo verificar el archivo guardado")
      }
      invoke.resolve(JSObject())
    } catch (error: Exception) {
      target?.let { runCatching { DocumentsContract.deleteDocument(activity.contentResolver, it) } }
      invoke.reject(error.message ?: "No se pudo publicar la descarga")
    }
  }

  @Command
  fun writeDocument(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(WriteArgs::class.java)
      activity.contentResolver.openOutputStream(Uri.parse(args.uri), "wt").use { output ->
        if (output == null) throw IllegalStateException("Android no concedió permiso de escritura")
        output.write(hexDecode(args.data))
      }
      invoke.resolve(JSObject())
    } catch (error: Exception) {
      invoke.reject(error.message ?: "No se pudo escribir el documento")
    }
  }

  @Command
  fun deleteVerifiedDocument(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(VerifiedDeleteArgs::class.java)
      val uri = Uri.parse(args.uri)
      if (documentInfo(uri) == null) {
        invoke.resolve(JSObject().apply { put("deleted", false) })
        return
      }
      val verified = try {
        hashContentUri(uri)
      } catch (_: java.io.FileNotFoundException) {
        invoke.resolve(JSObject().apply { put("deleted", false) })
        return
      }
      if (verified.first != args.size || verified.second != args.sha256) {
        throw IllegalStateException("El original cambió después de la subida; se conservó por seguridad.")
      }
      val deleted = runCatching { DocumentsContract.deleteDocument(activity.contentResolver, uri) }.getOrDefault(false) ||
        activity.contentResolver.delete(uri, null, null) > 0
      if (!deleted) throw IllegalStateException("Android no permitió borrar el original verificado")
      invoke.resolve(JSObject().apply { put("deleted", true) })
    } catch (error: Exception) {
      invoke.reject(error.message ?: "No se pudo borrar el original verificado")
    }
  }

  @Command
  fun backgroundApp(invoke: Invoke) {
    activity.moveTaskToBack(true)
    invoke.resolve(JSObject())
  }

  @Command
  fun updateSyncNotification(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(NotificationArgs::class.java)
      val detail = when (args.phase) {
        "starting" -> "Preparando sincronización en segundo plano…"
        "folders" -> "Cargando carpetas en segundo plano…"
        "files" -> if ((args.scanned ?: 0) > 0) {
          "Sincronizando archivos en segundo plano · ${args.scanned} encontrados"
        } else {
          "Sincronizando archivos en segundo plano…"
        }
        "applying" -> "Actualizando catálogo en segundo plano…"
        "error" -> args.error ?: "La sincronización no pudo completarse"
        else -> if (args.active) "Sincronizando con Telegram en segundo plano…" else "Sincronización completada"
      }
      updateNotification(SYNC_NOTIFICATION_ID, "Nuvio · Sincronización", detail, args.percent, args.active)
      invoke.resolve(JSObject())
    } catch (error: Exception) { invoke.reject(error.message ?: "No se pudo actualizar la notificación") }
  }

  @Command
  fun updateUploadNotification(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(NotificationArgs::class.java)
      val detail = args.currentFileName?.let { "Subiendo $it" } ?: if (args.active) "Subiendo archivos…" else "Subidas completadas"
      updateNotification(UPLOAD_NOTIFICATION_ID, "Nuvio · Subidas", detail, args.percent, args.active)
      invoke.resolve(JSObject())
    } catch (error: Exception) { invoke.reject(error.message ?: "No se pudo actualizar la notificación") }
  }

  @Command
  fun clearNotification(invoke: Invoke) {
    val args = invoke.parseArgs(NotificationIdArgs::class.java)
    notificationManager().cancel(args.id)
    invoke.resolve(JSObject())
  }

  private data class DocInfo(val id: String, val name: String, val mime: String, val size: Long)
  private data class ScanCounters(var files: Int = 0, var folders: Int = 0)

  private fun walkTree(
    tree: Uri,
    documentId: String,
    prefix: String,
    depth: Int,
    counters: ScanCounters,
    folders: MutableList<String>,
    files: JSArray,
    addBytes: (Long) -> Unit,
  ) {
    if (depth > MAX_TREE_DEPTH) {
      throw IllegalStateException("La carpeta seleccionada supera la profundidad máxima admitida por Nuvio")
    }
    for (child in listChildrenById(tree, documentId)) {
      val relative = if (prefix.isEmpty()) child.name else "$prefix/${child.name}"
      if (child.mime == DocumentsContract.Document.MIME_TYPE_DIR) {
        counters.folders += 1
        if (counters.folders > MAX_TREE_FOLDERS) {
          throw IllegalStateException("La carpeta contiene demasiadas subcarpetas para procesarla de forma segura")
        }
        folders.add(relative)
        walkTree(tree, child.id, relative, depth + 1, counters, folders, files, addBytes)
      } else {
        counters.files += 1
        if (counters.files > MAX_TREE_FILES) {
          throw IllegalStateException("Selecciona una carpeta con máximo 10000 archivos por operación")
        }
        val uri = DocumentsContract.buildDocumentUriUsingTree(tree, child.id)
        files.put(JSObject().apply {
          put("relativePath", relative)
          put("absolutePath", uri.toString())
          put("size", child.size)
        })
        addBytes(child.size)
      }
    }
  }

  private fun listChildren(tree: Uri): List<DocInfo> = listChildrenById(tree, DocumentsContract.getTreeDocumentId(tree))

  private fun listChildrenById(tree: Uri, parentId: String): List<DocInfo> {
    val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parentId)
    val projection = arrayOf(
      DocumentsContract.Document.COLUMN_DOCUMENT_ID,
      DocumentsContract.Document.COLUMN_DISPLAY_NAME,
      DocumentsContract.Document.COLUMN_MIME_TYPE,
      DocumentsContract.Document.COLUMN_SIZE,
    )
    val result = mutableListOf<DocInfo>()
    activity.contentResolver.query(childrenUri, projection, null, null, null)?.use { cursor ->
      while (cursor.moveToNext()) {
        result.add(DocInfo(cursor.getString(0), cursor.getString(1) ?: "archivo", cursor.getString(2) ?: "application/octet-stream", if (cursor.isNull(3)) 0 else cursor.getLong(3)))
      }
    }
    return result
  }

  private fun documentInfo(uri: Uri): DocInfo? {
    val projection = arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE, DocumentsContract.Document.COLUMN_SIZE)
    activity.contentResolver.query(uri, projection, null, null, null)?.use { cursor ->
      if (cursor.moveToFirst()) return DocInfo(cursor.getString(0), cursor.getString(1) ?: "archivo", cursor.getString(2) ?: "application/octet-stream", if (cursor.isNull(3)) 0 else cursor.getLong(3))
    }
    return null
  }

  private fun createDocument(tree: Uri, desiredName: String, policy: String): Uri {
    val parent = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
    val existing = listChildren(tree).map { it.name }.toHashSet()
    var name = safeName(desiredName)
    if (name in existing) {
      if (policy == "skip") throw IllegalStateException("El archivo ya existe en la carpeta seleccionada")
      val dot = name.lastIndexOf('.')
      val stem = if (dot > 0) name.substring(0, dot) else name
      val ext = if (dot > 0) name.substring(dot) else ""
      var index = 1
      do { name = "$stem ($index)$ext"; index++ } while (name in existing)
    }
    return DocumentsContract.createDocument(activity.contentResolver, parent, "application/octet-stream", name)
      ?: throw IllegalStateException("No se pudo crear el archivo en la carpeta seleccionada")
  }

  private fun displayName(uri: Uri): String? {
    activity.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
      if (cursor.moveToFirst()) return cursor.getString(0)
    }
    return uri.lastPathSegment
  }

  private fun persistPermissions(uri: Uri, flags: Int) {
    val take = flags and (Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
    runCatching { activity.contentResolver.takePersistableUriPermission(uri, take) }
  }

  private fun hashContentUri(uri: Uri): Pair<Long, String> {
    val digest = MessageDigest.getInstance("SHA-256")
    var size = 0L
    activity.contentResolver.openInputStream(uri).use { input ->
      if (input == null) throw IllegalStateException("Android no pudo abrir el archivo seleccionado")
      val buffer = ByteArray(1024 * 1024)
      while (true) {
        val read = input.read(buffer)
        if (read < 0) break
        if (read == 0) continue
        digest.update(buffer, 0, read)
        size += read
      }
    }
    return size to digest.digest().joinToString("") { "%02x".format(it) }
  }


  private fun getOrCreateSecretKey(): SecretKey {
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
    val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
    generator.init(KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT).setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).setRandomizedEncryptionRequired(true).build())
    return generator.generateKey()
  }

  private fun encryptSecret(input: ByteArray): ByteArray {
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(Cipher.ENCRYPT_MODE, getOrCreateSecretKey())
    return cipher.iv + cipher.doFinal(input)
  }

  private fun decryptSecret(input: ByteArray): ByteArray {
    if (input.size < 13) throw IllegalArgumentException("Secreto cifrado inválido")
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(Cipher.DECRYPT_MODE, getOrCreateSecretKey(), GCMParameterSpec(128, input.copyOfRange(0, 12)))
    return cipher.doFinal(input.copyOfRange(12, input.size))
  }

  private fun updateNotification(id: Int, title: String, text: String, percent: Int?, active: Boolean) {
    val manager = notificationManager()
    ensureChannel(manager)
    if (!active) {
      manager.cancel(id)
      return
    }
    if (Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
      if (!notificationPermissionRequested) {
        notificationPermissionRequested = true
        activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), NOTIFICATION_PERMISSION_REQUEST)
      }
      return
    }
    val launch = activity.packageManager.getLaunchIntentForPackage(activity.packageName)
    val pending = launch?.let { PendingIntent.getActivity(activity, id, it, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE) }
    val builder = android.app.Notification.Builder(activity, CHANNEL_ID)
    builder.setSmallIcon(R.mipmap.ic_launcher).setContentTitle(title).setContentText(text).setOngoing(true).setOnlyAlertOnce(true)
    if (pending != null) builder.setContentIntent(pending)
    if (percent != null) builder.setProgress(100, percent.coerceIn(0, 100), false) else builder.setProgress(0, 0, true)
    manager.notify(id, builder.build())
  }

  private fun notificationManager(): NotificationManager = activity.getSystemService(Activity.NOTIFICATION_SERVICE) as NotificationManager

  private fun ensureChannel(manager: NotificationManager) {
    if (manager.getNotificationChannel(CHANNEL_ID) == null) {
      manager.createNotificationChannel(NotificationChannel(CHANNEL_ID, "Transferencias Nuvio", NotificationManager.IMPORTANCE_LOW))
    }
  }

  private fun safeName(value: String): String {
    val clean = value.replace('/', '_').replace('\\', '_').trim()
    if (clean.isEmpty() || clean == "." || clean == "..") return "archivo"
    return clean.take(180)
  }

  private fun hexDecode(value: String): ByteArray {
    require(value.length % 2 == 0) { "Hex inválido" }
    return ByteArray(value.length / 2) { index -> value.substring(index * 2, index * 2 + 2).toInt(16).toByte() }
  }

  private fun hexEncode(value: ByteArray): String = value.joinToString("") { "%02x".format(it) }
}
