package com.telly.player

import android.app.Activity
import android.content.pm.ActivityInfo
import android.view.View
import android.view.WindowManager
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class ImmersiveArgs {
  var on: Boolean = false
}

/** What Telly's pages ask of Android itself: the player's whole screen. */
@TauriPlugin
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
      content.rootView?.let { ViewCompat.requestApplyInsets(it) }
      invoke.resolve(JSObject())
    }
  }
}
