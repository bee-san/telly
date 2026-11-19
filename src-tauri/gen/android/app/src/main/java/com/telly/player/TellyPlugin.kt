package com.telly.player

import android.app.Activity
import android.content.Intent
import android.content.pm.ActivityInfo
import android.view.View
import android.view.WindowManager
import androidx.core.content.FileProvider
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.ichi2.anki.api.AddContentApi
import java.io.File

@InvokeArg
class ImmersiveArgs {
  var on: Boolean = false
}

/** AnkiDroid's permission to add notes. */
private const val ANKI_PERMISSION = "com.ichi2.anki.permission.READ_WRITE_DATABASE"

/**
 * What Telly's pages ask of Android itself: the player's whole screen, the
 * clip of a line cut by FFmpegKit, and cards added to AnkiDroid.
 */
@TauriPlugin(permissions = [Permission(strings = [ANKI_PERMISSION], alias = "anki")])
class TellyPlugin(private val activity: Activity) : Plugin(activity) {
  /** The player takes the whole screen, sideways, with the screen kept on; or gives it back. */
  @Command
  fun immersive(invoke: Invoke) {
    val on = invoke.parseArgs(ImmersiveArgs::class.java).on
    activity.runOnUiThread {
      (activity as? MainActivity)?.wholeScreen = on
      val window = activity.window
      val controller = WindowCompat.getInsetsController(window, window.decorView)
      if (on) {
        controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        controller.hide(WindowInsetsCompat.Type.systemBars())
        activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
      } else {
        controller.show(WindowInsetsCompat.Type.systemBars())
        activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
      }
      val content = activity.findViewById<View>(android.R.id.content)
      ViewCompat.requestApplyInsets(content)
      invoke.resolve(JSObject())
    }
  }

  /** Whether AnkiDroid is installed, Telly may add notes to it, and FFmpegKit is in this build to cut clips. */
  @Command
  fun ankiState(invoke: Invoke) {
    val result = JSObject()
    result.put("installed", AddContentApi.getAnkiDroidPackageName(activity) != null)
    result.put("allowed", getPermissionState("anki") == PermissionState.GRANTED)
    result.put("clips", ffmpegKit() != null)
    invoke.resolve(result)
  }

  /** Asks you to let Telly add notes to AnkiDroid. */
  @Command
  fun ankiAllow(invoke: Invoke) {
    if (getPermissionState("anki") == PermissionState.GRANTED) ankiState(invoke)
    else requestPermissionForAlias("anki", invoke, "ankiAllowed")
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
        if (AddContentApi.getAnkiDroidPackageName(activity) == null) return@Thread invoke.reject("AnkiDroid isn’t installed")
        if (getPermissionState("anki") != PermissionState.GRANTED) return@Thread invoke.reject("Telly isn’t allowed to add cards to AnkiDroid yet")
        val api = AddContentApi(activity)
        val modelName = args.getString("model")
        val modelId = api.modelList?.entries?.firstOrNull { it.value == modelName }?.key
          ?: return@Thread invoke.reject("AnkiDroid has no note type called $modelName. Sync AnkiDroid with AnkiWeb so it has the one your PC uses.")
        val deckName = args.getString("deck")
        val deckId = api.deckList?.entries?.firstOrNull { it.value == deckName }?.key ?: api.addNewDeck(deckName)
          ?: return@Thread invoke.reject("AnkiDroid didn’t make the deck $deckName")
        val names = api.getFieldList(modelId) ?: return@Thread invoke.reject("AnkiDroid didn’t say what fields $modelName has")
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
            activity.grantUriPermission("com.ichi2.anki", uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            val added = api.addMediaFromUri(uri, file.nameWithoutExtension, item.getString("kind")) ?: continue
            val field = item.getString("field").lowercase()
            values[field] = (values[field] ?: "") + added
          }
        }
        val fields = names.map { values[it.lowercase()] ?: "" }.toTypedArray()
        val tagList = args.optJSONArray("tags")
        val tags = HashSet<String>()
        if (tagList != null) for (i in 0 until tagList.length()) tags.add(tagList.getString(i))
        val noteId = api.addNote(modelId, deckId, fields, tags)
          ?: return@Thread invoke.reject("AnkiDroid didn’t add the note (is the first field empty, or the same as another note’s?)")
        val result = JSObject()
        result.put("note", noteId)
        invoke.resolve(result)
      } catch (e: Exception) {
        invoke.reject("AnkiDroid: ${e.message}")
      }
    }.start()
  }
}
