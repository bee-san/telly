package com.telly.player

import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import kotlin.math.abs

/**
 * A controller's buttons and sticks, told to Telly's pages by name
 * (src/lib/pad.ts): `press` when one goes down or comes up, `scroll` with how
 * far the right stick is pushed up (-1) or down (1).
 *
 * A D-pad comes as keys or as the HAT axes, and triggers as keys or axes,
 * depending on the controller: both are taken, and a button held is said once.
 * The left stick pushed past half-way is four buttons of its own.
 */
class Pad(private val press: (String, Boolean) -> Unit, private val scroll: (Float) -> Unit) {
  private val held = HashSet<String>()
  private var hat: String? = null
  private var stick: String? = null
  private var leftTrigger = false
  private var rightTrigger = false
  private var scrolling = 0f

  private fun hold(name: String, down: Boolean) {
    if (down) {
      if (held.add(name)) press(name, true)
    } else if (held.remove(name)) press(name, false)
  }

  /** Returns true when the key was a controller's, and so used. */
  fun key(event: KeyEvent): Boolean {
    val name = when (event.keyCode) {
      KeyEvent.KEYCODE_DPAD_UP -> "up"
      KeyEvent.KEYCODE_DPAD_DOWN -> "down"
      KeyEvent.KEYCODE_DPAD_LEFT -> "left"
      KeyEvent.KEYCODE_DPAD_RIGHT -> "right"
      KeyEvent.KEYCODE_BUTTON_A, KeyEvent.KEYCODE_DPAD_CENTER -> "a"
      KeyEvent.KEYCODE_BUTTON_B -> "b"
      KeyEvent.KEYCODE_BUTTON_X -> "x"
      KeyEvent.KEYCODE_BUTTON_Y -> "y"
      KeyEvent.KEYCODE_BUTTON_L1 -> "l1"
      KeyEvent.KEYCODE_BUTTON_R1 -> "r1"
      KeyEvent.KEYCODE_BUTTON_L2 -> "l2"
      KeyEvent.KEYCODE_BUTTON_R2 -> "r2"
      KeyEvent.KEYCODE_BUTTON_START -> "start"
      KeyEvent.KEYCODE_BUTTON_SELECT -> "select"
      else -> return KeyEvent.isGamepadButton(event.keyCode)
    }
    if (event.action == KeyEvent.ACTION_DOWN) hold(name, true)
    else if (event.action == KeyEvent.ACTION_UP) hold(name, false)
    return true
  }

  /** Returns true when the movement was a controller's stick, D-pad or trigger, and so used. */
  fun motion(event: MotionEvent): Boolean {
    if (event.source and InputDevice.SOURCE_CLASS_JOYSTICK == 0 || event.action != MotionEvent.ACTION_MOVE) return false
    val hatX = event.getAxisValue(MotionEvent.AXIS_HAT_X)
    val hatY = event.getAxisValue(MotionEvent.AXIS_HAT_Y)
    val nextHat = when {
      hatX <= -0.5f -> "left"
      hatX >= 0.5f -> "right"
      hatY <= -0.5f -> "up"
      hatY >= 0.5f -> "down"
      else -> null
    }
    if (nextHat != hat) {
      hat?.let { hold(it, false) }
      hat = nextHat
      nextHat?.let { hold(it, true) }
    }
    val x = event.getAxisValue(MotionEvent.AXIS_X)
    val y = event.getAxisValue(MotionEvent.AXIS_Y)
    val nextStick = when {
      x <= -STICK && -x >= abs(y) -> "stick-left"
      x >= STICK && x >= abs(y) -> "stick-right"
      y <= -STICK -> "stick-up"
      y >= STICK -> "stick-down"
      else -> null
    }
    if (nextStick != stick) {
      stick?.let { hold(it, false) }
      stick = nextStick
      nextStick?.let { hold(it, true) }
    }
    // Triggers that are axes: some controllers name them BRAKE and GAS.
    val left = maxOf(event.getAxisValue(MotionEvent.AXIS_LTRIGGER), event.getAxisValue(MotionEvent.AXIS_BRAKE)) >= 0.5f
    val right = maxOf(event.getAxisValue(MotionEvent.AXIS_RTRIGGER), event.getAxisValue(MotionEvent.AXIS_GAS)) >= 0.5f
    if (left != leftTrigger) {
      leftTrigger = left
      hold("l2", left)
    }
    if (right != rightTrigger) {
      rightTrigger = right
      hold("r2", right)
    }
    // Controllers disagree on where the right stick's vertical axis is: RZ is the common one, RY the other.
    val rz = event.getAxisValue(MotionEvent.AXIS_RZ)
    val pushed = if (abs(rz) > 0.2f) rz else event.getAxisValue(MotionEvent.AXIS_RY)
    val speed = if (abs(pushed) > 0.2f) Math.round(pushed * 10) / 10f else 0f
    if (speed != scrolling) {
      scrolling = speed
      scroll(speed)
    }
    return true
  }

  /** Telly is no longer in front: nothing stays held. */
  fun release() {
    for (name in held.toList()) hold(name, false)
    hat = null
    stick = null
    leftTrigger = false
    rightTrigger = false
    if (scrolling != 0f) {
      scrolling = 0f
      scroll(0f)
    }
  }

  private companion object {
    const val STICK = 0.6f
  }
}
