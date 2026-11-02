package app.portalinbox.ui

import android.content.Intent
import android.content.res.ColorStateList
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.data.KeyVault
import app.portalinbox.data.Store
import app.portalinbox.net.EnrollField
import app.portalinbox.net.Enroller
import app.portalinbox.net.EnrollmentError
import app.portalinbox.service.InboxService

/**
 * Enrolls this phone with a one-time enrollment code made by an account of the portal.
 * The code is typed or pasted here. A tapped `portal-inbox://enroll?...` link opens this
 * screen with the code filled in. The screen shows the server address and a short
 * fingerprint of the server key that the code pins, so the person can check them.
 */
class AddServerActivity : BaseActivity() {
    private lateinit var code: EditText
    private lateinit var name: EditText
    private lateinit var check: LinearLayout
    private val errors = mutableMapOf<EditText, TextView>()
    private lateinit var connect: LinearLayout
    private lateinit var connectLabel: TextView
    private lateinit var progress: ProgressBar
    private lateinit var status: TextView
    private var busy = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val root = lin()
        title = getString(R.string.add_title)
        root.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }))
        val body = lin(padH = 10, padV = 4)
        root.addView(scroll(body), lp(MATCH, 0, 1f))

        body.addView(sectionTitle(getString(R.string.add_title)))
        val form = panel(padH = 14, padV = 12)
        code = textField("", getString(R.string.add_code_hint), mono = true).apply {
            isSingleLine = false
            minLines = 2
            maxLines = 5
            gravity = Gravity.TOP or Gravity.START
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or
                InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or InputType.TYPE_TEXT_VARIATION_URI
            imeOptions = EditorInfo.IME_ACTION_NEXT
            textSize = SP_S
        }
        field(form, R.string.add_code, code, help = getString(R.string.add_code_help))
        check = lin().apply { visibility = View.GONE }
        form.addView(check, lp().margins(this, t = 14))
        name = textField(Build.MODEL, "")
        field(form, R.string.add_device_name, name, help = getString(R.string.add_device_name_help))
        body.addView(form)

        body.addView(lin(vertical = false, padH = 4, padV = 10).apply {
            addView(icon(R.drawable.ic_info, color(R.color.text_muted), 16), lp(dp(16), dp(16)).margins(this@AddServerActivity, r = 8))
            addView(label(getString(R.string.add_security_note), SP_S, color(R.color.text_muted)), lp(0, WRAP, 1f))
        })

        connect = connectButton()
        body.addView(connect, lp().margins(this, t = 4))
        status = label("", SP_S, color(R.color.danger)).apply {
            setPadding(dp(4), dp(10), dp(4), dp(10))
            visibility = View.GONE
            accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
        }
        body.addView(status)
        setScreen(root)

        code.addTextChangedListener(object : android.text.TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun afterTextChanged(s: android.text.Editable?) { refreshCheck() }
        })
        name.setOnEditorActionListener { _, action, _ ->
            if (action == EditorInfo.IME_ACTION_DONE) { submit(); true } else false
        }
        prefill(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        prefill(intent)
    }

    /** `portal-inbox://enroll?...`: fills the code. The person still checks the server and taps Connect. */
    private fun prefill(intent: Intent?) {
        val data = intent?.data ?: return
        if (data.scheme != "portal-inbox" || data.host != "enroll") return
        code.setText(data.toString())
        clearErrors()
        if (refreshCheck()) {
            name.requestFocus()
            name.setSelection(name.text.length)
        } else {
            showError(code, getString(R.string.err_code_invalid))
        }
    }

    /** Shows the server address and the key fingerprint of the code, when the code is valid. */
    private fun refreshCheck(): Boolean {
        check.removeAllViews()
        val text = code.text.toString()
        val info = if (text.isBlank()) null else try { Enroller.parse(this, text) } catch (e: EnrollmentError) { null }
        if (info == null) {
            check.visibility = View.GONE
            return false
        }
        check.addView(label(getString(R.string.add_check_title), SP_S, color(R.color.text), MEDIUM))
        check.addView(label(getString(R.string.add_check_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 2))
        val rows = listOf(
            kv(getString(R.string.meta_address), info.address.http.removePrefix("http://").removePrefix("https://"), color(R.color.text), 2, true),
            kv(getString(R.string.add_check_fingerprint), info.fingerprint, color(R.color.text), 1, true),
        )
        for (row in rows) {
            row.setPadding(0, dp(5), 0, dp(5))
            check.addView(row, if (row === rows[0]) lp().margins(this, t = 6) else lp())
        }
        check.visibility = View.VISIBLE
        return true
    }

    /** Field label, the input (or a box holding it), and an error line shown under it when needed. */
    private fun field(form: LinearLayout, title: Int, view: View, input: EditText = view as EditText, help: String? = null) {
        val top = if (form.childCount == 0) 0 else 14
        if (input.id == View.NO_ID) input.id = View.generateViewId()
        form.addView(label(getString(title), SP_S, color(R.color.text), MEDIUM).apply { labelFor = input.id }, lp().margins(this, t = top))
        help?.let { form.addView(label(it, SP_S, color(R.color.text_muted)), lp().margins(this, t = 2)) }
        form.addView(view, lp().margins(this, t = 6))
        val error = label("", SP_S, color(R.color.danger)).apply {
            visibility = View.GONE
            accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
        }
        form.addView(error, lp().margins(this, t = 4))
        errors[input] = error
        input.addTextChangedListener(object : android.text.TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun afterTextChanged(s: android.text.Editable?) { if (!busy) hideError(input) }
        })
    }

    /** Primary button whose progress spinner sits inside it while connecting. */
    private fun connectButton(): LinearLayout = lin(vertical = false).apply {
        gravity = Gravity.CENTER
        minimumHeight = dp(48)
        background = pressable(color(R.color.primary), dpf(CONTROL_RADIUS))
        isClickable = true
        isFocusable = true
        progress = ProgressBar(this@AddServerActivity).apply {
            isIndeterminate = true
            indeterminateTintList = ColorStateList.valueOf(color(R.color.on_primary))
            visibility = View.GONE
        }
        addView(progress, lp(dp(18), dp(18)).margins(this@AddServerActivity, r = 10))
        connectLabel = label(getString(R.string.add_connect), SP_M, color(R.color.on_primary), MEDIUM)
        addView(connectLabel)
        setOnClickListener { submit() }
    }

    private fun setBusy(on: Boolean) {
        busy = on
        connect.isEnabled = !on
        connect.alpha = if (on) 0.85f else 1f
        progress.visibility = if (on) View.VISIBLE else View.GONE
        connectLabel.text = getString(if (on) R.string.add_connecting else R.string.add_connect)
        connect.contentDescription = connectLabel.text
        for (f in listOf(code, name)) f.isEnabled = !on
    }

    private fun showError(field: EditText, text: String) {
        errors[field]?.apply {
            this.text = text
            visibility = View.VISIBLE
        }
        field.background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.danger), dp(1))
        field.requestFocus()
    }

    private fun hideError(field: EditText) {
        val e = errors[field] ?: return
        if (e.visibility == View.GONE) return
        e.visibility = View.GONE
        field.background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
    }

    private fun clearErrors() {
        errors.keys.forEach { hideError(it) }
        status.visibility = View.GONE
    }

    private fun submit() {
        if (busy) return
        clearErrors()
        val text = code.text.toString().trim()
        if (text.isEmpty()) return showError(code, getString(R.string.add_code_missing))
        val info = try { Enroller.parse(this, text) } catch (e: EnrollmentError) { return showError(code, e.message ?: "") }
        val deviceName = name.text.toString().trim()
        if (deviceName.isEmpty()) return showError(name, getString(R.string.add_device_name_missing))
        setBusy(true)
        Thread {
            var serverId = -1L
            val error: EnrollmentError? = try {
                val e = Enroller.enroll(this, text, deviceName)
                // The key of the code is the pin: the stored key is the one of the code.
                if (!e.serverPublicKey.contentEquals(info.serverPublicKey)) throw EnrollmentError(getString(R.string.err_server_key))
                val sealed = KeyVault.seal(e.devicePrivateKey)
                e.devicePrivateKey.fill(0)
                serverId = Store.get(this).addServer(e.address.http, e.serverName, e.deviceId, deviceName, sealed, e.serverPublicKey, e.userName)
                null
            } catch (e: EnrollmentError) {
                e
            } catch (e: Exception) {
                EnrollmentError(getString(R.string.add_error, e.message ?: e.javaClass.simpleName))
            }
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                setBusy(false)
                if (error == null) done(serverId) else fail(error)
            }
        }.start()
    }

    private fun fail(e: EnrollmentError) {
        val text = e.message ?: getString(R.string.err_unknown)
        when (e.field) {
            EnrollField.CODE -> showError(code, text)
            EnrollField.DEVICE_NAME -> showError(name, text)
            EnrollField.NONE -> status.apply {
                this.text = text
                visibility = View.VISIBLE
            }
        }
    }

    /** Enrolled: straight to the groups of the server. The code has no use any more. */
    private fun done(serverId: Long) {
        code.text.clear()
        InboxService.start(this)
        startActivities(arrayOf(
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            Intent(this, ServerActivity::class.java).putExtra(ServerActivity.EXTRA_SERVER, serverId),
        ))
        finish()
    }
}
