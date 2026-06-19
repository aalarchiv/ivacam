package org.opcenter.ivacam

import android.os.Bundle
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback

// Tauri 2.11.0's generated TauriActivity wires every PluginManager lifecycle
// hook (onResume/onPause/onStop/onNewIntent/onRestart/onDestroy/…) EXCEPT
// onActivityCreate() — and nothing in tauri/wry/tao calls it either. That is
// the only place the plugin activity-result launchers are registered via
// registerForActivityResult (file dialogs, permission requests). Without it
// PluginManager.startActivityForResultLauncher stays uninitialized and every
// file dialog fails natively with "lateinit property
// startActivityForResultLauncher has not been initialized" — swallowed by the
// dialog plugin's Rust layer into a silent null (ivac-0gu0).
//
// Register it ourselves, AFTER super.onCreate() (ComponentActivity sets up its
// ActivityResultRegistry there) and BEFORE the activity reaches STARTED (still
// in onCreate, so registerForActivityResult is legal). The call is guarded and
// idempotent (PluginManager.onActivityCreate early-returns once bound).
class MainActivity : TauriActivity() {
  // Take over Android system-back handling (ivac-h0ai). WryActivity's default
  // OnBackPressedCallback finishes the activity once the webview can't go
  // back — an edge-swipe quits the app abruptly from the root screen. We
  // disable that here and route every back gesture to the frontend instead
  // (see onWebViewCreate), so the JS layer decides navigate-to-first-screen
  // vs. confirm-exit.
  override val handleBackNavigation: Boolean = false

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    getPluginManager().onActivityCreate(this)
  }

  // Called by WryActivity.setWebView once the RustWebView exists. Register our
  // own back callback here so we have a webview reference to dispatch into.
  // Instead of finish(), we fire a DOM CustomEvent the frontend listens for
  // (App.svelte 'android-back'); native never quits the app directly — the
  // frontend drives the exit via @tauri-apps/plugin-process. Using a plain
  // DOM event (not a Tauri event) keeps this off the version-fragile Rust
  // event plumbing. Predictive-back edge info (Android 14+ swipeEdge) isn't
  // forwarded yet — every back maps to "first screen, then confirm-exit".
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    val callback = object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        webView.evaluateJavascript(
          "window.dispatchEvent(new CustomEvent('android-back'))",
          null,
        )
      }
    }
    onBackPressedDispatcher.addCallback(this, callback)
  }
}
