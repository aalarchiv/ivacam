package org.opcenter.ivacam

import android.os.Bundle

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
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    getPluginManager().onActivityCreate(this)
  }
}
