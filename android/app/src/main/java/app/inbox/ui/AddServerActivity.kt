package app.inbox.ui

import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.widget.TextView
import app.inbox.R
import app.inbox.data.KeyVault
import app.inbox.data.Store
import app.inbox.net.Enroller
import app.inbox.net.EnrollmentError
import app.inbox.net.ServerAddress
import app.inbox.service.InboxService

/** Enrolls this phone on a server with the shared device password and the user's pseudo. */
class AddServerActivity : BaseActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val root = lin()
        title = getString(R.string.add_title)
        root.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }))
        val body = lin(padH = 10, padV = 4)
        root.addView(scroll(body), lp(MATCH, 0, 1f))

        body.addView(sectionLabel(getString(R.string.add_title), getString(R.string.add_encrypted)))
        val form = panel(padH = 14, padV = 12)
        fun field(title: Int, help: String? = null) {
            form.addView(label(getString(title).uppercase(), 10.5f, color(R.color.text_muted), MEDIUM).apply { letterSpacing = 0.1f },
                lp().margins(this, t = if (form.childCount == 0) 0 else 12))
            help?.let { form.addView(label(it, 11f, color(R.color.text_subtle)), lp().margins(this, t = 2)) }
        }
        field(R.string.add_address)
        val address = textField("", getString(R.string.add_address_hint), mono = true).apply {
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
        }
        form.addView(address, lp().margins(this, t = 6))
        field(R.string.add_password)
        val password = textField("", "", password = true)
        form.addView(password, lp().margins(this, t = 6))
        field(R.string.add_pseudo)
        val pseudo = textField(Prefs.defaultUserName(this) ?: "", getString(R.string.pseudo_hint), mono = true)
        form.addView(pseudo, lp().margins(this, t = 6))
        field(R.string.add_device_name, getString(R.string.add_device_name_help))
        val name = textField(Build.MODEL, "", mono = true)
        form.addView(name, lp().margins(this, t = 6))
        body.addView(form)

        body.addView(lin(vertical = false, padH = 6, padV = 10).apply {
            addView(icon(R.drawable.ic_info, color(R.color.text_subtle), 14), lp(dp(14), dp(14)).margins(this@AddServerActivity, r = 8))
            addView(label(getString(R.string.add_security_note), 11f, color(R.color.text_muted)), lp(0, WRAP, 1f))
        })

        val status = mono("", 11.5f, color(R.color.text_muted), lines = 4).apply { setPadding(dp(6), dp(10), dp(6), dp(10)) }
        lateinit var connect: TextView
        connect = button(getString(R.string.add_connect)) {
            val addr = ServerAddress.parse(address.text.toString())
            if (addr == null) {
                status.setTextColor(color(R.color.danger))
                status.text = getString(R.string.add_invalid_address)
                return@button
            }
            val user = cleanUserName(pseudo.text.toString())
            if (user == null) {
                pseudo.error = getString(R.string.pseudo_invalid)
                return@button
            }
            val pw = password.text.toString().toByteArray(Charsets.UTF_8)
            password.text.clear()
            val deviceName = name.text.toString().trim()
            connect.isEnabled = false
            connect.alpha = 0.6f
            status.setTextColor(color(R.color.text_muted))
            status.text = getString(R.string.add_connecting)
            Thread {
                val result = try {
                    val e = Enroller.enroll(addr, pw, deviceName, user)
                    val sealed = KeyVault.seal(e.devicePrivateKey)
                    e.devicePrivateKey.fill(0)
                    Store.get(this).addServer(addr.http, e.serverName, e.deviceId, deviceName, sealed, e.serverPublicKey, user)
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
                        connect.alpha = 1f
                        status.setTextColor(color(R.color.danger))
                        status.text = result
                    }
                }
            }.start()
        }
        body.addView(connect, lp().margins(this, t = 4))
        body.addView(status)
        setScreen(root)

        if (Prefs.defaultUserName(this) == null) {
            status.setTextColor(color(R.color.warning))
            status.text = getString(R.string.add_pseudo_required)
        }
    }
}
