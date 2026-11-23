package com.telly.player

import android.os.Bundle
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

class MainActivity : TauriActivity() {
  /** Whether the player has the whole screen, its cutout too. */
  var wholeScreen = false
  /** The WebView Telly's pages are in, once there is one. */
  var page: WebView? = null
    private set
  /** A controller's buttons, told to the page (src/lib/pad.ts). */
  private val pad = Pad(
    { button, down -> tell("telly-pad", "{button:'$button',down:$down}") },
    { speed -> tell("telly-pad", "{button:'scroll',speed:$speed}") },
  )
  /** Telly on the device's other screen, when it has one. */
  val second by lazy { SecondScreen(this) }

  /** Tells the page something happened, as an event on its window with `detail` (JavaScript) as its detail. */
  fun tell(event: String, detail: String) {
    page?.evaluateJavascript("window.dispatchEvent(new CustomEvent('$event',{detail:$detail}))", null)
  }

  override fun onWebViewCreate(webView: WebView) {
    page = webView
    // The page keeps the focus through a touch: without it, Android spends the next press of the D-pad on giving the focus back, and the page never hears it.
    webView.isFocusableInTouchMode = true
    webView.requestFocus()
    second.refresh()
  }

  override fun onWindowFocusChanged(hasFocus: Boolean) {
    super.onWindowFocusChanged(hasFocus)
    if (hasFocus) {
      page?.requestFocus()
      hideBars()
    }
    second.keys = !hasFocus
  }

  override fun dispatchKeyEvent(event: KeyEvent): Boolean = pad.key(event) || super.dispatchKeyEvent(event)

  override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean = padMotion(event) || super.dispatchGenericMotionEvent(event)

  /** A controller's stick moved, wherever Android sent it. */
  fun padMotion(event: MotionEvent): Boolean = pad.motion(event)

  override fun onStart() {
    super.onStart()
    second.start()
  }

  override fun onStop() {
    super.onStop()
    second.stop()
  }

  override fun onPause() {
    super.onPause()
    pad.release()
  }

  /** Telly has the whole screen: the status and navigation bars are hidden, and a swipe from an edge shows them for a moment. */
  fun hideBars() {
    val controller = WindowCompat.getInsetsController(window, window.decorView)
    controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    controller.hide(WindowInsetsCompat.Type.systemBars())
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    hideBars()
    // Telly's pages keep clear of a camera's cutout and the keyboard, but for the player's.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime())
      if (wholeScreen) view.setPadding(0, 0, 0, 0) else view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }
}
