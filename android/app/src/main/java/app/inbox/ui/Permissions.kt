package app.inbox.ui

import android.Manifest
import android.app.Activity
import android.app.NotificationManager
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import app.inbox.R

/** One system permission Inbox needs to ring reliably. */
data class Permission(
    val key: String,
    val title: Int,
    val why: Int,
    val granted: Boolean,
    val request: (Activity) -> Unit,
)

object Permissions {
    fun all(a: Activity): List<Permission> {
        val nm = a.getSystemService(NotificationManager::class.java)
        val pm = a.getSystemService(PowerManager::class.java)
        val pkg = Uri.parse("package:${a.packageName}")
        val list = mutableListOf<Permission>()
        if (Build.VERSION.SDK_INT >= 33) {
            list += Permission(
                "notifications", R.string.perm_notifications,
                R.string.perm_notifications_why,
                a.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED,
            ) { it.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1) }
        }
        list += Permission(
            "overlay", R.string.perm_overlay,
            R.string.perm_overlay_why,
            Settings.canDrawOverlays(a),
        ) { it.startActivity(Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION, pkg)) }
        if (Build.VERSION.SDK_INT >= 34) {
            list += Permission(
                "full_screen", R.string.perm_full_screen,
                R.string.perm_full_screen_why,
                nm.canUseFullScreenIntent(),
            ) { it.startActivity(Intent(Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, pkg)) }
        }
        list += Permission(
            "battery", R.string.perm_battery,
            R.string.perm_battery_why,
            pm.isIgnoringBatteryOptimizations(a.packageName),
        ) { it.startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, pkg)) }
        list += Permission(
            "dnd", R.string.perm_dnd,
            R.string.perm_dnd_why,
            nm.isNotificationPolicyAccessGranted,
        ) { it.startActivity(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS)) }
        return list
    }

    fun missing(a: Activity): List<Permission> = all(a).filter { !it.granted }
}
