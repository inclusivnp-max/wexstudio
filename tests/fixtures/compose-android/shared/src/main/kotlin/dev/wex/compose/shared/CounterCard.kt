package dev.wex.compose.shared

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun CounterCard(onFrame: (Int) -> Unit) {
    var count by remember { mutableIntStateOf(0) }
    LaunchedEffect(count) { onFrame(count) }
    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
        Column(
            modifier = Modifier.fillMaxSize().padding(28.dp),
            verticalArrangement = Arrangement.spacedBy(24.dp, Alignment.CenterVertically),
            horizontalAlignment = Alignment.CenterHorizontally
        ) {
            Text("Wex Compose", style = MaterialTheme.typography.headlineLarge)
            Text("Native Kotlin · Java · APK", style = MaterialTheme.typography.titleMedium)
            Text(CounterLabel.value(count), style = MaterialTheme.typography.displayMedium)
            Button(
                onClick = { count += 1 },
                shapes = ButtonDefaults.shapes(),
                modifier = Modifier.semantics { contentDescription = "wex-increment" }
            ) { Text("Increment") }
            Text("Built without Gradle", style = MaterialTheme.typography.bodyLarge)
        }
    }
}
