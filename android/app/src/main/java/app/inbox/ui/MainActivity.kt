package app.inbox.ui

import android.Manifest
import android.app.Activity
import android.app.NotificationManager
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.widget.LinearLayout
import app.inbox.BuildConfig
import app.inbox.data.Store
import app.inbox.service.LinkState
import app.inbox.service.InboxService
import app.inbox.service.Registry

/** Server list, connection status and the permissions Inbox needs to be reliable. */
class MainActivity : Activity() {
    private lateinit var root: LinearLayout
    private val handler = Handler(Looper.getMainLooper())
    private val onChange: () -> Unit = { render() }
    private val refresh = object : Runnable {
        override fun run() {
            render()
            handler.postDelayed(this, 5_000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        root = column()
        setContentView(scroll(root))
        InboxService.start(this)
    }

    override fun onResume() {
        super.onResume()
        Registry.addListener(onChange)
        handler.post(refresh)
        InboxService.start(this)
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
        handler.removeCallbacks(refresh)
    }

    private fun render() {
        root.removeAllViews()
        root.addView(text("Inbox", 26f, bold = true))
        root.addView(text("Réception d'alertes uniquement. Version ${BuildConfig.VERSION_NAME}", 13f, color = GREY))
        renderPermissions()
        root.addView(text("Serveurs", 20f, bold = true).apply { setPadding(0, dp(16), 0, dp(4)) })
        val servers = Store.get(this).servers()
        if (servers.isEmpty()) {
            root.addView(text("Aucun serveur. Ajoutez-en un pour recevoir des alertes.", color = GREY))
        }
        val now = System.currentTimeMillis()
        for (s in servers) {
            val st = Registry.status(s.id)
            val (label, color) = when (st.state) {
                LinkState.CONNECTED -> "Connecté" to GREEN
                LinkState.CONNECTING -> "Connexion…" to ORANGE
                LinkState.DISCONNECTED -> "Déconnecté ${formatAgo(now - st.sinceMs)}" to RED
                LinkState.REENROLL -> "Ré-enrôlement requis" to RED
            }
            val c = card(color)
            c.addView(text(s.serverName, 18f, bold = true))
            c.addView(text(s.address, 13f, color = GREY))
            c.addView(text(label, bold = true, color = color))
            if (st.state != LinkState.CONNECTED && st.detail != null) c.addView(text(st.detail, 13f, color = GREY))
            c.setOnClickListener {
                startActivity(Intent(this, ServerActivity::class.java).putExtra(ServerActivity.EXTRA_SERVER, s.id))
            }
            root.addView(c)
        }
        root.addView(button("Ajouter un serveur") { startActivity(Intent(this, AddServerActivity::class.java)) })
    }

    private fun renderPermissions() {
        val nm = getSystemService(NotificationManager::class.java)
        val pm = getSystemService(PowerManager::class.java)
        val missing = mutableListOf<Pair<String, () -> Unit>>()
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            missing += "Autoriser les notifications (indispensable)" to {
                requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
            }
        }
        if (Build.VERSION.SDK_INT >= 34 && !nm.canUseFullScreenIntent()) {
            missing += "Autoriser les alarmes plein écran" to {
                startActivity(Intent(Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, Uri.parse("package:$packageName")))
            }
        }
        if (!pm.isIgnoringBatteryOptimizations(packageName)) {
            missing += "Désactiver l'optimisation de batterie (sinon Android peut couper Inbox)" to {
                startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
            }
        }
        if (!nm.isNotificationPolicyAccessGranted) {
            missing += "Autoriser l'accès « Ne pas déranger » (pour sortir du silencieux)" to {
                startActivity(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS))
            }
        }
        if (missing.isEmpty()) {
            root.addView(text("Toutes les autorisations sont accordées.", 13f, color = GREEN))
            return
        }
        val c = card(RED)
        c.addView(text("Autorisations manquantes", 16f, bold = true, color = RED))
        c.addView(text("Sans elles, une alerte critique pourrait ne pas sonner.", 13f))
        for ((label, action) in missing) c.addView(button(label, action))
        root.addView(c)
    }
}
