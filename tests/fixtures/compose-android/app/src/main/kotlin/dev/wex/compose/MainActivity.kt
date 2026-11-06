package dev.wex.compose

import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialExpressiveTheme
import dev.wex.compose.shared.CounterCard

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val runId = intent.getStringExtra("wex.run_id") ?: "manual"
        setContent {
            MaterialExpressiveTheme {
                CounterCard { count ->
                    Log.i("WexRun", "WEX_COMPOSE_FRAME count=$count runId=$runId")
                }
            }
        }
        Log.i("WexRun", "WEX_RUN_OK runId=$runId compose=true")
    }
    override fun onResume() {
        super.onResume()
        Log.i("WexRun", "WEX_ACTIVITY_RESUMED runId=${intent.getStringExtra("wex.run_id")}")
    }
}

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@androidx.compose.runtime.Composable
fun WexPreview() {
    MaterialExpressiveTheme(colorScheme = if (androidx.compose.foundation.isSystemInDarkTheme()) androidx.compose.material3.darkColorScheme() else androidx.compose.material3.lightColorScheme()) {
        androidx.compose.foundation.layout.Box {
            CounterCard { }
            androidx.compose.material3.Text(
                androidx.compose.ui.res.stringResource(R.string.app_name),
                color = androidx.compose.material3.MaterialTheme.colorScheme.onSurface,
                modifier = androidx.compose.ui.Modifier
            )
        }
    }
}
