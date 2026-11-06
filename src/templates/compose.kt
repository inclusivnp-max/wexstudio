package __PACKAGE__

import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val runId = intent.getStringExtra("wex.run_id") ?: "manual"
        setContent { AppScreen { count ->
            Log.i("WexRun", "WEX_COMPOSE_FRAME count=$count runId=$runId")
        } }
        Log.i("WexRun", "WEX_RUN_OK runId=$runId")
    }
    override fun onResume() {
        super.onResume()
        Log.i("WexRun", "WEX_ACTIVITY_RESUMED runId=${intent.getStringExtra("wex.run_id")}")
    }
}

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun AppScreen(onFrame: (Int) -> Unit = {}) {
    MaterialExpressiveTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
        var count by remember { mutableIntStateOf(0) }
        LaunchedEffect(count) { onFrame(count) }
        Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
            Column(
                modifier = Modifier.fillMaxSize().padding(28.dp),
                verticalArrangement = Arrangement.spacedBy(24.dp, Alignment.CenterVertically),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                Text(stringResource(R.string.app_name), style = MaterialTheme.typography.headlineLarge)
                Text("$count", style = MaterialTheme.typography.displayLarge)
                Button(onClick = { count++ }, shapes = ButtonDefaults.shapes()) { Text("Increment") }
                Text("Built with Wex · without Gradle", style = MaterialTheme.typography.bodyLarge)
            }
        }
    }
}

@Composable
fun WexPreview() { AppScreen() }
