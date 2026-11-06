package __PACKAGE__

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.widget.TextView

class MainActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(TextView(this).apply {
            text = getString(R.string.app_name)
            textSize = 28f
            gravity = android.view.Gravity.CENTER
        })
        Log.i("WexRun", "WEX_RUN_OK runId=${intent.getStringExtra("wex.run_id")}")
    }
    override fun onResume() {
        super.onResume()
        Log.i("WexRun", "WEX_ACTIVITY_RESUMED runId=${intent.getStringExtra("wex.run_id")}")
    }
}
