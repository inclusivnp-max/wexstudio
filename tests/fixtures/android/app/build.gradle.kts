plugins { id("com.android.application") }
android {
    namespace = "dev.wex.android"
    compileSdk = 36
    defaultConfig { applicationId = "dev.wex.android.integration"; minSdk = 26; targetSdk = 36; versionCode = 1; versionName = "1.0" }
    buildFeatures { buildConfig = true }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
}
dependencies { implementation(project(":shared")); implementation("androidx.core:core:1.13.0") }
