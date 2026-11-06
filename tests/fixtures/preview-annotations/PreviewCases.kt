@file:JvmName("PreviewShowcase")
package dev.wex.compose

import androidx.compose.foundation.layout.padding
import androidx.compose.ui.unit.dp
import androidx.compose.runtime.Composable
import androidx.compose.ui.tooling.preview.Preview as Scene
import androidx.compose.ui.tooling.preview.PreviewLightDark

private const val WIDTH = 240
private const val HEIGHT = 320
private const val TITLE = "Direct"

@Scene(name = "MetaDay", group = "Multi", widthDp = WIDTH, heightDp = HEIGHT)
@Scene(name = "MetaNight", group = "Multi", widthDp = WIDTH, heightDp = HEIGHT,
    uiMode = android.content.res.Configuration.UI_MODE_NIGHT_YES, fontScale = 1.25f, locale = "fr-rFR")
@Repeatable
@Target(AnnotationTarget.FUNCTION, AnnotationTarget.ANNOTATION_CLASS)
@Retention(AnnotationRetention.BINARY)
annotation class DisplayModes

@DisplayModes
@Target(AnnotationTarget.FUNCTION)
@Retention(AnnotationRetention.BINARY)
annotation class NestedDisplay

@Scene(name = TITLE, group = "Phone", widthDp = WIDTH, heightDp = HEIGHT,
    showBackground = true, backgroundColor = 0xFF00FF00)
@Composable
private fun DirectScene() { androidx.compose.foundation.layout.Box(androidx.compose.ui.Modifier.padding(16.dp)) { WexPreview() } }

@Scene(name = "Direct", group = "Duplicate", widthDp = WIDTH, heightDp = HEIGHT)
@Composable
fun DuplicateDirectScene() { WexPreview() }

@Scene(name = "Repeated", group = "Phone", widthDp = WIDTH, heightDp = HEIGHT)
@Scene(name = "Landscape", group = "Phone", widthDp = HEIGHT * 2, heightDp = WIDTH,
    uiMode = android.content.res.Configuration.UI_MODE_NIGHT_YES, fontScale = 1.25f)
@Composable
fun RepeatedScene() { WexPreview() }

@NestedDisplay
@Composable
fun MultiScene() { WexPreview() }

@DisplayModes
@DisplayModes
@Composable
fun RepeatedMultiScene() { WexPreview() }

@PreviewLightDark
@Composable
fun LibraryScenes() { WexPreview() }

@Scene(name = "RequiredParameter", widthDp = WIDTH, heightDp = HEIGHT)
@Composable
fun NeedsArgument(text: String) { androidx.compose.material3.Text(text) }

@Scene(name = "SystemUi", showSystemUi = true)
@Composable
fun DecorScene() { WexPreview() }

@Scene(name = "OldApi", apiLevel = 30)
@Composable
fun OldPlatformScene() { WexPreview() }

// @Scene(name = "NotAnAnnotation") must never be discovered.
class InitializerProbe {
    companion object { init { error("DISCOVERY_EXECUTED_PROJECT_CODE") } }
}
