buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:9.3.1")
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.2.10")
    }
}

allprojects {
    repositories {
        google()
        mavenCentral()
        // AnkiDroid's API for adding cards (TellyPlugin.kt) is published on JitPack.
        maven("https://jitpack.io")
    }
}

tasks.register("clean").configure {
    delete("build")
}

