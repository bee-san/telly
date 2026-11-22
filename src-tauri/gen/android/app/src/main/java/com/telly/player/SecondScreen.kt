package com.telly.player

import android.annotation.SuppressLint
import android.app.Presentation
import android.graphics.Color
import android.hardware.display.DisplayManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Display
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.ViewGroup
import android.view.WindowManager
import android.webkit.JavascriptInterface
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

/**
 * Telly on a second screen, like the AYN Thor's bottom one: while Telly is in
 * front and the device has another screen, that screen shows Telly's page
 * /second (the lines of what's playing, what a word means, what to carry on
 * watching). The page on the main screen says what to show, and is told what
 * was touched (src/lib/second.svelte.ts). A phone with one screen gets
 * nothing but a listener for a screen being plugged in.
 */
class SecondScreen(private val activity: MainActivity) {
  private val displays = activity.getSystemService(DisplayManager::class.java)
  private val main = Handler(Looper.getMainLooper())
  private var shown: Presentation? = null
  private var web: WebView? = null
  /** The page on the second screen has loaded, and can be told things. */
  private var ready = false
  private var started = false
  /** Put away with its close button: it stays away until Telly is opened again. */
  private var putAway = false
  /** What the main page said to show since the second screen came up, by what it's about: told to the second page when it has loaded. */
  private val said = LinkedHashMap<String, String>()

  private val listener = object : DisplayManager.DisplayListener {
    override fun onDisplayAdded(displayId: Int) = update()
    override fun onDisplayRemoved(displayId: Int) = update()
    override fun onDisplayChanged(displayId: Int) = update()
  }

  /**
   * Whether it may take the focus, and with it the controller's keys, which it
   * hands to the main screen: only while the main screen's window hasn't the
   * focus (a screen just plugged in takes it for its own launcher), so the
   * controller is never left with nothing of Telly's.
   */
  var keys = false
    set(on) {
      field = on
      shown?.window?.let { if (on) it.clearFlags(WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE) else it.addFlags(WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE) }
    }

  /** Whether the second screen is showing Telly. */
  val showing get() = shown != null

  /** Telly came to the front. */
  fun start() {
    if (started) return
    started = true
    putAway = false
    displays.registerDisplayListener(listener, main)
    update()
  }

  /** Telly is no longer in front: the other screen is given back. */
  fun stop() {
    if (!started) return
    started = false
    displays.unregisterDisplayListener(listener)
    update()
  }

  /** The screen Telly isn't on, if there is one that's on and anyone's to draw on. */
  @Suppress("DEPRECATION")
  private fun other(): Display? {
    val mine = activity.windowManager.defaultDisplay.displayId
    return displays.displays.firstOrNull { it.displayId != mine && it.state != Display.STATE_OFF && it.flags and Display.FLAG_PRIVATE == 0 }
  }

  /** The main page's WebView is there now, or will be once it's been set up. */
  fun refresh() {
    update()
    if (started && !serving()) main.postDelayed({ refresh() }, 200)
  }

  /** Whether the main page's WebView can serve the second page: it has the client that answers for Telly's own addresses. */
  private fun serving(): Boolean {
    // Below Android 8 a WebView can't be asked for its client.
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return false
    val page = activity.page ?: return false
    return page.webViewClient.javaClass != WebViewClient::class.java
  }

  private fun update() {
    val display = if (started && !putAway && serving()) other() else null
    if (shown?.display?.displayId == display?.displayId) return
    hide()
    // The pages say it all again when the screen is back: what they'd said may be out of date by then.
    said.clear()
    if (display != null) show(display)
    activity.tell("telly-second", "{type:'${if (showing) "shown" else "hidden"}'}")
  }

  @SuppressLint("SetJavaScriptEnabled")
  private fun show(display: Display) {
    val page = activity.page ?: return
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val presentation = object : Presentation(activity, display) {
      override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Touched, it doesn't take the controller or the keyboard from the main screen.
        if (!keys) window?.addFlags(WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE)
      }

      // The keys it's given while the main screen hasn't the focus are the main screen's.
      override fun dispatchKeyEvent(event: KeyEvent): Boolean = activity.dispatchKeyEvent(event)

      override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean = activity.padMotion(event) || super.dispatchGenericMotionEvent(event)
    }
    val home = Uri.parse(page.url ?: "http://tauri.localhost/")
    val view = WebView(presentation.context)
    view.setBackgroundColor(Color.BLACK)
    // As the main page does, it keeps the focus through a touch, so the first press of the D-pad it's given isn't lost.
    view.isFocusableInTouchMode = true
    view.requestFocus()
    view.settings.javaScriptEnabled = true
    view.settings.domStorageEnabled = true
    // Telly's pages, fonts and covers come from the app itself, by way of the main page's WebView.
    val client = page.webViewClient
    view.webViewClient = object : WebViewClient() {
      override fun shouldInterceptRequest(v: WebView, request: WebResourceRequest): WebResourceResponse? = client.shouldInterceptRequest(page, request)

      // It stays on Telly's own page: a link in a definition goes nowhere.
      override fun shouldOverrideUrlLoading(v: WebView, request: WebResourceRequest): Boolean = request.url.authority != home.authority
    }
    view.addJavascriptInterface(Bridge(view), "TellySecond")
    // The page sits clear of that screen's own bars, where it has any.
    val frame = FrameLayout(presentation.context)
    frame.setBackgroundColor(Color.BLACK)
    frame.addView(view, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
    ViewCompat.setOnApplyWindowInsetsListener(frame) { v, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
    presentation.setContentView(frame)
    // Android takes it down when its screen goes or changes shape: it's put back if the screen is still there.
    presentation.setOnDismissListener {
      if (shown === presentation) {
        forget()
        main.post { update() }
      }
    }
    try {
      presentation.show()
    } catch (e: WindowManager.InvalidDisplayException) {
      view.destroy()
      return
    }
    shown = presentation
    web = view
    view.loadUrl("${home.scheme}://${home.authority}/second")
  }

  private fun forget() {
    shown = null
    ready = false
    web?.let {
      (it.parent as? ViewGroup)?.removeView(it)
      it.destroy()
    }
    web = null
  }

  private fun hide() {
    val presentation = shown ?: return
    shown = null
    presentation.dismiss()
    forget()
  }

  /**
   * Shows `json` on the second screen as what it has about `about` ("lines",
   * "look"…). Kept, it's told again whenever the second page loads.
   */
  fun say(about: String, json: String, keep: Boolean) {
    if (keep) said[about] = json
    if (ready) tell(about, json)
  }

  private fun tell(about: String, json: String) {
    web?.evaluateJavascript("window.__tellySecond&&window.__tellySecond('$about',$json)", null)
  }

  /** What the second page calls: it has loaded, something on it was touched, or its close button was. */
  private inner class Bridge(private val view: WebView) {
    @JavascriptInterface
    fun ready() = main.post {
      if (web !== view) return@post
      ready = true
      for ((about, json) in said) tell(about, json)
    }

    @JavascriptInterface
    fun post(json: String) = main.post {
      if (web === view) activity.tell("telly-second", json)
    }

    @JavascriptInterface
    fun close() = main.post {
      if (web !== view) return@post
      putAway = true
      update()
    }
  }
}
