package app.inbox.ui

import android.app.Activity
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import app.inbox.data.KeyVault
import app.inbox.data.Store
import app.inbox.net.Enroller
import app.inbox.net.EnrollmentError
import app.inbox.net.ServerAddress
import app.inbox.service.InboxService

/** Enrolls this phone on a server with the shared device password. */
class AddServerActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        title = "Ajouter un serveur"
        val root = column()
        root.addView(text("Adresse du serveur", bold = true))
        val address = EditText(this).apply {
            hint = "ex. 192.168.1.10:8080 ou https://inbox.example.com"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
            isSingleLine = true
        }
        root.addView(address)
        root.addView(text("Mot de passe des appareils", bold = true))
        val password = EditText(this).apply {
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            isSingleLine = true
        }
        root.addView(password)
        root.addView(text("Nom de cet appareil (visible par l'administrateur)", bold = true))
        val name = EditText(this).apply {
            setText(Build.MODEL)
            isSingleLine = true
        }
        root.addView(name)
        root.addView(
            text(
                "Le mot de passe ne quitte jamais le téléphone (protocole OPAQUE), même sur une connexion http non chiffrée.",
                12f, color = GREY,
            )
        )
        val status = TextView(this).apply { setPadding(0, dp(8), 0, dp(8)) }
        lateinit var connect: Button
        connect = button("Connecter") {
            val addr = ServerAddress.parse(address.text.toString())
            if (addr == null) {
                status.setTextColor(RED)
                status.text = "Adresse invalide"
                return@button
            }
            val pw = password.text.toString().toByteArray(Charsets.UTF_8)
            password.text.clear()
            val deviceName = name.text.toString().trim()
            connect.isEnabled = false
            status.setTextColor(GREY)
            status.text = "Connexion et vérification du mot de passe…"
            Thread {
                val result = try {
                    val e = Enroller.enroll(addr, pw, deviceName)
                    val sealed = KeyVault.seal(e.devicePrivateKey)
                    e.devicePrivateKey.fill(0)
                    Store.get(this).addServer(addr.http, e.serverName, e.deviceId, deviceName, sealed, e.serverPublicKey)
                    null
                } catch (e: EnrollmentError) {
                    e.message
                } catch (e: Exception) {
                    "Erreur : ${e.message ?: e.javaClass.simpleName}"
                }
                runOnUiThread {
                    if (result == null) {
                        InboxService.start(this)
                        finish()
                    } else {
                        connect.isEnabled = true
                        status.setTextColor(RED)
                        status.text = result
                    }
                }
            }.start()
        }
        root.addView(connect)
        root.addView(status)
        setContentView(scroll(root))
    }
}
