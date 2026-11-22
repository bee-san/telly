package com.telly.player

import android.os.Bundle
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  /** Whether the player has the whole screen, under the status and navigation bars. */
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
    if (hasFocus) page?.requestFocus()
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

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Telly's pages sit between the status bar and the navigation bar, but for the player's.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime())
      if (wholeScreen) view.setPadding(0, 0, 0, 0) else view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }
}
