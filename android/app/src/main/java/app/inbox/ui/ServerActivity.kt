package app.inbox.ui

import android.app.Activity
import android.app.AlertDialog
import android.os.Bundle
import android.view.View
import android.widget.AdapterView
import android.widget.ArrayAdapter
import android.widget.LinearLayout
import android.widget.Spinner
import android.widget.Toast
import app.inbox.alert.Alerter
import app.inbox.data.Store
import app.inbox.service.LinkState
import app.inbox.service.InboxService
import app.inbox.service.Registry

/** Groups and per-group action of one server, and its recent alerts. */
class ServerActivity : Activity() {
    companion object {
        const val EXTRA_SERVER = "server_id"
        private val ACTIONS = listOf<String?>(null, "notify", "ring", "alarm")
    }

    private var serverId = -1L
    private lateinit var root: LinearLayout
    private val onChange: () -> Unit = { render() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        root = column()
        setContentView(scroll(root))
    }

    override fun onResume() {
        super.onResume()
        Registry.addListener(onChange)
        render()
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
    }

    private fun render() {
        val store = Store.get(this)
        val server = store.server(serverId) ?: return finish()
        title = server.serverName
        root.removeAllViews()
        val st = Registry.status(serverId)
        root.addView(text(server.serverName, 22f, bold = true))
        root.addView(text("${server.address} · appareil « ${server.deviceName} »", 13f, color = GREY))
        val (label, color) = when (st.state) {
            LinkState.CONNECTED -> "Connecté" to GREEN
            LinkState.CONNECTING -> "Connexion…" to ORANGE
            LinkState.DISCONNECTED -> "Déconnecté" to RED
            LinkState.REENROLL -> "Ré-enrôlement requis" to RED
        }
        root.addView(text(label + (st.detail?.let { " — $it" } ?: ""), bold = true, color = color))

        root.addView(text("Groupes", 18f, bold = true).apply { setPadding(0, dp(12), 0, dp(4)) })
        root.addView(text("Choisissez ce que fait le téléphone quand une alerte du groupe arrive.", 13f, color = GREY))
        val subs = store.subscriptions(serverId)
        val groups = store.groups(serverId)
        if (groups.isEmpty()) root.addView(text("Connectez-vous une première fois pour voir les groupes.", color = GREY))
        for (g in groups) {
            val c = card()
            c.addView(text(g.name, 16f, bold = true))
            if (g.description.isNotEmpty()) c.addView(text(g.description, 13f, color = GREY))
            val spinner = Spinner(this)
            spinner.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item, ACTIONS.map { actionLabel(it) })
            val current = ACTIONS.indexOf(subs[g.id]).coerceAtLeast(0)
            spinner.setSelection(current, false)
            spinner.onItemSelectedListener = object : AdapterView.OnItemSelectedListener {
                override fun onItemSelected(parent: AdapterView<*>?, view: View?, position: Int, id: Long) {
                    if (position == current) return
                    val ok = Registry.connections[serverId]?.subscribe(g.id, ACTIONS[position]) ?: false
                    if (!ok) {
                        Toast.makeText(this@ServerActivity, "Hors ligne : réessayez une fois connecté", Toast.LENGTH_LONG).show()
                        spinner.setSelection(current, false)
                    }
                }

                override fun onNothingSelected(parent: AdapterView<*>?) {}
            }
            c.addView(spinner)
            root.addView(c)
        }

        root.addView(text("Alertes récentes", 18f, bold = true).apply { setPadding(0, dp(12), 0, dp(4)) })
        val alerts = store.recentAlerts(serverId, 50)
        if (alerts.isEmpty()) root.addView(text("Aucune alerte reçue.", color = GREY))
        for (a in alerts) {
            val c = card(if (a.action == "alarm" && a.ackedAtMs == null) RED else null)
            c.addView(text(a.title ?: a.groupName, 16f, bold = true))
            a.message?.let { c.addView(text(it)) }
            c.addView(text("${a.groupName} · ${actionLabel(a.action)} · ${formatTime(a.createdAtMs)}", 12f, color = GREY))
            if (a.ackedAtMs == null) {
                c.addView(button("Acquitter") { Alerter.acknowledge(this, a.serverId, a.alertId) })
            } else {
                c.addView(text("Acquittée ${formatTime(a.ackedAtMs)}", 12f, color = GREEN))
            }
            root.addView(c)
        }

        root.addView(button("Supprimer ce serveur") {
            AlertDialog.Builder(this)
                .setTitle("Supprimer ${server.serverName} ?")
                .setMessage("Ce téléphone ne recevra plus ses alertes. Il faudra le mot de passe pour l'ajouter à nouveau.")
                .setPositiveButton("Supprimer") { _, _ ->
                    Registry.connections.remove(serverId)?.stop()
                    store.deleteServer(serverId)
                    Registry.forget(serverId)
                    InboxService.start(this)
                    finish()
                }
                .setNegativeButton("Annuler", null)
                .show()
        })
    }
}
