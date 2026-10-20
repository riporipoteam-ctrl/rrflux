plugins {
    id("com.android.application")
}

android {
    namespace = "com.ripoteam.launcher"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.ripoteam.launcher"
        minSdk = 26
        targetSdk = 35
        versionCode = 2
        versionName = "0.2.0"

        val firebaseKey = System.getenv("FIREBASE_WEB_API_KEY") ?: ""
        buildConfigField("String", "FIREBASE_WEB_API_KEY", "\"$firebaseKey\"")
    }

    buildFeatures {
        buildConfig = true
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
