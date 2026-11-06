package dev.wex

// Kotlin editor fixture; not an Android build fixture yet.
class MainActivity {
    private val greeting = "Hello from WexAndroidStudio"

    fun welcome(name: String): String {
        return "$greeting, $name!"
    }
}
