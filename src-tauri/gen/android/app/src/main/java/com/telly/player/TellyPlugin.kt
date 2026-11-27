package com.telly.player

import android.app.Activity
import android.content.Intent
import android.content.pm.ActivityInfo
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.view.View
import android.view.WindowManager
import android.view.inputmethod.InputMethodManager
import android.webkit.WebView
import androidx.core.content.FileProvider
import androidx.core.view.ViewCompat
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File

@InvokeArg
class ImmersiveArgs {
  var on: Boolean = false
}

/** AnkiDroid's and kuma3's permissions to add notes (AnkiApp.kt). */
private const val ANKI_PERMISSION = "com.ichi2.anki.permission.READ_WRITE_DATABASE"
private const val KUMA3_PERMISSION = "io.github.pedrubik2000.kuma3.permission.READ_WRITE_DATABASE"

@InvokeArg
class SecondArgs {
  var about: String = ""
  var json: String = "null"
  var keep: Boolean = true
}

@InvokeArg
class InstallArgs {
  var path: String = ""
}

/**
 * What Telly's pages ask of Android itself: the player's whole screen, the
 * clip of a line cut by FFmpegKit, cards added to AnkiDroid, a newer Telly
 * installed, Android's keyboard for a controller, and what the second screen
 * shows.
 */
@TauriPlugin(permissions = [Permission(strings = [ANKI_PERMISSION], alias = "anki"), Permission(strings = [KUMA3_PERMISSION], alias = "kuma3")])
class TellyPlugin(private val activity: Activity) : Plugin(activity) {
  private var page: WebView? = null

  override fun load(webView: WebView) {
    page = webView
  }

  /** The player takes the whole screen, sideways, with the screen kept on; or gives it back. Android's bars stay hidden either way (MainActivity). */
  @Command
  fun immersive(invoke: Invoke) {
    val on = invoke.parseArgs(ImmersiveArgs::class.java).on
    activity.runOnUiThread {
      (activity as? MainActivity)?.let {
        it.wholeScreen = on
        it.hideBars()
      }
      val window = activity.window
      if (on) {
        activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
      } else {
        activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
      }
      val content = activity.findViewById<View>(android.R.id.content)
      ViewCompat.requestApplyInsets(content)
      invoke.resolve(JSObject())
    }
  }

  /** Brings up Android's keyboard for the field a controller pressed A on, which a touch does by itself. */
  @Command
  fun keyboard(invoke: Invoke) {
    activity.runOnUiThread {
      // Until it's touched, the page isn't what Android's keyboard types into.
      page?.let {
        it.requestFocus()
        activity.getSystemService(InputMethodManager::class.java).showSoftInput(it, 0)
      }
      invoke.resolve(JSObject())
    }
  }

  /** Whether Telly is on a second screen (SecondScreen.kt). */
  @Command
  fun secondState(invoke: Invoke) {
    activity.runOnUiThread {
      val result = JSObject()
      result.put("on", (activity as? MainActivity)?.second?.showing == true)
      invoke.resolve(result)
    }
  }

  /** Shows `json` on the second screen as what it has about `about`; kept, it's shown again when that screen comes back. */
  @Command
  fun second(invoke: Invoke) {
    val args = invoke.parseArgs(SecondArgs::class.java)
    activity.runOnUiThread {
      (activity as? MainActivity)?.second?.say(args.about, args.json, args.keep)
      invoke.resolve(JSObject())
    }
  }

  /** Whether AnkiDroid (or kuma3, AnkiApp.kt) is installed, Telly may add notes to it, and FFmpegKit is in this build to cut clips. */
  @Command
  fun ankiState(invoke: Invoke) {
    val app = AnkiApp.find(activity)
    val result = JSObject()
    result.put("installed", app != null)
    result.put("app", app?.name ?: "AnkiDroid")
    result.put("allowed", app != null && getPermissionState(app.alias) == PermissionState.GRANTED)
    result.put("clips", ffmpegKit() != null)
    invoke.resolve(result)
  }

  /** Asks you to let Telly add notes to AnkiDroid (or kuma3). */
  @Command
  fun ankiAllow(invoke: Invoke) {
    val app = AnkiApp.find(activity)
    if (app == null || getPermissionState(app.alias) == PermissionState.GRANTED) ankiState(invoke)
    else requestPermissionForAlias(app.alias, invoke, "ankiAllowed")
  }

  @PermissionCallback
  fun ankiAllowed(invoke: Invoke) = ankiState(invoke)

  /** FFmpegKit's class, when this build has it (its .aar in app/libs). */
  private fun ffmpegKit(): Class<*>? = try {
    Class.forName("com.arthenica.ffmpegkit.FFmpegKit")
  } catch (e: ClassNotFoundException) {
    null
  }

  /** Runs ffmpeg with each of `runs` (lists of arguments), one after the other. */
  @Command
  fun clip(invoke: Invoke) {
    val kit = ffmpegKit() ?: return invoke.reject("This Telly can’t cut clips: it was built without FFmpegKit")
    val runs = invoke.getArgs().getJSONArray("runs")
    Thread {
      try {
        val execute = kit.getMethod("executeWithArguments", Array<String>::class.java)
        val codes = Class.forName("com.arthenica.ffmpegkit.ReturnCode")
        val isSuccess = codes.getMethod("isSuccess", codes)
        for (i in 0 until runs.length()) {
          val list = runs.getJSONArray(i)
          val args = Array(list.length()) { list.getString(it) }
          val session = execute.invoke(null, args as Any)
          val code = session.javaClass.getMethod("getReturnCode").invoke(session)
          if (isSuccess.invoke(null, code) != true) {
            val output = session.javaClass.getMethod("getOutput").invoke(session) as? String ?: ""
            val last = output.lines().lastOrNull { it.isNotBlank() } ?: "it failed"
            return@Thread invoke.reject("ffmpeg: $last")
          }
        }
        invoke.resolve(JSObject())
      } catch (e: Exception) {
        invoke.reject("ffmpeg: ${e.cause?.message ?: e.message}")
      }
    }.start()
  }

  /**
   * Adds a note to AnkiDroid: `model` and `deck` by name (the deck is made
   * if it isn't there), `fields` by name, `tags`, and `media` ([{field, path,
   * kind}]) added to AnkiDroid's media and put in their fields.
   */
  @Command
  fun addCard(invoke: Invoke) {
    val args = invoke.getArgs()
    Thread {
      try {
        val app = AnkiApp.find(activity) ?: return@Thread invoke.reject("AnkiDroid isn’t installed")
        val anki = app.name
        if (getPermissionState(app.alias) != PermissionState.GRANTED) return@Thread invoke.reject("Telly isn’t allowed to add cards to $anki yet")
        val r = activity.contentResolver
        val modelName = args.getString("model")
        val modelId = app.models(r)?.entries?.firstOrNull { it.value == modelName }?.key
          ?: return@Thread invoke.reject("$anki has no note type called $modelName. Sync $anki with AnkiWeb so it has the one your PC uses.")
        val deckName = args.getString("deck")
        val deckId = app.decks(r)?.entries?.firstOrNull { it.value == deckName }?.key ?: app.addDeck(r, deckName)
          ?: return@Thread invoke.reject("$anki didn’t make the deck $deckName")
        val names = app.fieldNames(r, modelId) ?: return@Thread invoke.reject("$anki didn’t say what fields $modelName has")
        val given = args.getJSONObject("fields")
        val values = HashMap<String, String>()
        for (key in given.keys()) values[key.lowercase()] = given.getString(key)
        val media = args.optJSONArray("media")
        if (media != null) {
          for (i in 0 until media.length()) {
            val item = media.getJSONObject(i)
            val kept = File(item.getString("path"))
            if (!kept.isFile) continue
            // Shared from the cache, the folder FileProvider lends out (file_paths.xml).
            val file = File(File(activity.cacheDir, "cards").apply { mkdirs() }, kept.name)
            kept.copyTo(file, overwrite = true)
            val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", file)
            activity.grantUriPermission(app.packageName, uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            val added = app.addMedia(r, uri, file.nameWithoutExtension, item.getString("kind")) ?: continue
            val field = item.getString("field").lowercase()
            values[field] = (values[field] ?: "") + added
          }
        }
        val fields = names.map { values[it.lowercase()] ?: "" }
        val tagList = args.optJSONArray("tags")
        val tags = HashSet<String>()
        if (tagList != null) for (i in 0 until tagList.length()) tags.add(tagList.getString(i))
        val noteId = app.addNote(r, modelId, deckId, fields, tags)
          ?: return@Thread invoke.reject("$anki didn’t add the note (is the first field empty, or the same as another note’s?)")
        val result = JSObject()
        result.put("note", noteId)
        invoke.resolve(result)
      } catch (e: Exception) {
        invoke.reject("${AnkiApp.find(activity)?.name ?: "AnkiDroid"}: ${e.message}")
      }
    }.start()
  }

  /**
   * Hands the newer Telly's APK (`path`, downloaded by the backend) to
   * Android's installer, which asks you to update. The first time, Android
   * asks you to let Telly install apps: allowed, press Install again.
   */
  @Command
  fun installApk(invoke: Invoke) {
    val kept = File(invoke.parseArgs(InstallArgs::class.java).path)
    if (!kept.isFile) return invoke.reject("The newer Telly didn’t download")
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !activity.packageManager.canRequestPackageInstalls()) {
      activity.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")))
      return invoke.reject("Let Telly install apps, then press Install again")
    }
    Thread {
      try {
        // Shared from the cache, the folder FileProvider lends out (file_paths.xml).
        val file = File(File(activity.cacheDir, "update").apply { deleteRecursively(); mkdirs() }, "Telly.apk")
        kept.copyTo(file, overwrite = true)
        kept.delete()
        val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", file)
        val install = Intent(Intent.ACTION_VIEW)
          .setDataAndType(uri, "application/vnd.android.package-archive")
          .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        activity.runOnUiThread {
          activity.startActivity(install)
          invoke.resolve(JSObject())
        }
      } catch (e: Exception) {
        invoke.reject("Android’s installer didn’t open: ${e.message}")
      }
    }.start()
  }
}
