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
  private var page: WebView? = null
  /** A controller's buttons, told to the page (src/lib/pad.ts). */
  private val pad = Pad(
    { button, down -> tell("{button:'$button',down:$down}") },
    { speed -> tell("{button:'scroll',speed:$speed}") },
  )

  private fun tell(detail: String) {
    page?.evaluateJavascript("window.dispatchEvent(new CustomEvent('telly-pad',{detail:$detail}))", null)
  }

  override fun onWebViewCreate(webView: WebView) {
    page = webView
  }

  override fun dispatchKeyEvent(event: KeyEvent): Boolean = pad.key(event) || super.dispatchKeyEvent(event)

  override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean = pad.motion(event) || super.dispatchGenericMotionEvent(event)

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
