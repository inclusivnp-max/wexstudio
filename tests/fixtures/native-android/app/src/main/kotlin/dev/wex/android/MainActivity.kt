package dev.wex.android

import android.app.Activity
import android.os.Bundle
import android.widget.TextView
import android.util.Log
import android.os.Process
import dev.wex.shared.Greeter
import androidx.core.content.ContextCompat

class MainActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val greeter = Greeter()
        val title = getString(R.string.app_name)
        val debug = BuildConfig.DEBUG
        val nativeProof = JavaGreeting.decorate(greeter.greeting(title))
        val sharedProof = getString(dev.wex.shared.R.string.shared_proof)
        val assetProof = assets.open("wex-native-proof.txt").bufferedReader().use { it.readText().trim() }
        debugProbe(7)
        Log.i("WexRun", "WEX_NATIVE_OK runId=${intent.getStringExtra("wex.run_id")} java=$nativeProof resource=$sharedProof asset=$assetProof")
        Log.i("WexRun", "WEX_RUN_OK runId=${intent.getStringExtra("wex.run_id")} package=$packageName pid=${Process.myPid()} greeting=${greeter.greeting(title)}")
        setContentView(TextView(this).apply {
            setTextColor(ContextCompat.getColor(this@MainActivity, android.R.color.black))
            text = nativeProof + " / " + sharedProof + if (debug) " [debug]" else ""
        })
    }

    private fun debugProbe(seed: Int): Int {
        val doubled = seed * 2
        val samples = IntArray(250) { it * 3 }
        val answer = debugNested(doubled)
        Log.i("WexDebug", "WEX_DEBUG_OK result=$answer runId=${intent.getStringExtra("wex.run_id")}")
        return answer
    }

    private fun debugNested(value: Int): Int {
        val adjusted = value + 3
        return adjusted
    }

    override fun onResume() {
        super.onResume()
        Log.i("WexRun", "WEX_ACTIVITY_RESUMED runId=${intent.getStringExtra("wex.run_id")} pid=${Process.myPid()}")
    }
}
