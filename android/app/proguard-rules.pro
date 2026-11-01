# JNI entry points are resolved by name from libinbox_android.so.
-keep class app.inbox.core.Native { *; }
-keepclasseswithmembernames class * { native <methods>; }
