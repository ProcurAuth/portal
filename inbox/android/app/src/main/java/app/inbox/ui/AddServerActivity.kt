package app.inbox.ui

import android.content.Intent
import android.content.res.ColorStateList
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import app.inbox.R
import app.inbox.data.KeyVault
import app.inbox.data.Store
import app.inbox.net.EnrollField
import app.inbox.net.Enroller
import app.inbox.net.EnrollmentError
import app.inbox.net.ServerAddress
import app.inbox.service.InboxService

/**
 * Enrolls this phone on a server with the shared device password and the user's
 * username. Also opened by `inbox://add?url=<encoded http(s) base>`, which only
 * prefills the address: the password is still typed here.
 */
class AddServerActivity : BaseActivity() {
    private lateinit var address: EditText
    private lateinit var password: EditText
    private lateinit var pseudo: EditText
    private lateinit var name: EditText
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
        address = textField("", getString(R.string.add_address_hint), mono = true).apply {
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
            imeOptions = EditorInfo.IME_ACTION_NEXT
        }
        field(form, R.string.add_address, address)
        password = textField("", "", password = true).apply { imeOptions = EditorInfo.IME_ACTION_NEXT }
        field(form, R.string.add_password, passwordBox(password), password)
        val default = Prefs.defaultUserName(this)
        pseudo = textField(default ?: "", getString(R.string.pseudo_hint), mono = true).apply { imeOptions = EditorInfo.IME_ACTION_NEXT }
        field(form, R.string.add_pseudo, pseudo, help = if (default == null) getString(R.string.add_pseudo_first) else null)
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

    /** `inbox://add?url=…`: prefills the address when it is a plain http(s) base. */
    private fun prefill(intent: Intent?) {
        val data = intent?.data ?: return
        if (data.scheme != "inbox" || data.host != "add") return
        val raw = data.getQueryParameter("url")?.trim().orEmpty()
        val scheme = Uri.parse(raw).scheme?.lowercase()
        val addr = if (scheme == "http" || scheme == "https") ServerAddress.parse(raw) else null
        if (addr == null) {
            showError(address, getString(R.string.add_link_invalid))
            return
        }
        address.setText(addr.http)
        clearErrors()
        password.requestFocus()
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

    /** Password input with a show / hide toggle at its end. */
    private fun passwordBox(input: EditText): FrameLayout {
        val box = FrameLayout(this)
        input.setPadding(input.paddingLeft, input.paddingTop, dp(48), input.paddingBottom)
        box.addView(input, FrameLayout.LayoutParams(MATCH, WRAP))
        var shown = false
        lateinit var toggle: FrameLayout
        fun refresh() {
            val pos = input.selectionEnd
            input.inputType = InputType.TYPE_CLASS_TEXT or
                if (shown) InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD else InputType.TYPE_TEXT_VARIATION_PASSWORD
            input.typeface = REGULAR
            // Changing the input type resets the line count, which shares its field with minHeight.
            input.minHeight = dp(48)
            if (pos >= 0) input.setSelection(pos.coerceAtMost(input.text.length))
            toggle.removeAllViews()
            toggle.addView(icon(if (shown) R.drawable.ic_eye_off else R.drawable.ic_eye, color(R.color.text_muted), 20),
                FrameLayout.LayoutParams(dp(20), dp(20), Gravity.CENTER))
            toggle.contentDescription = getString(if (shown) R.string.add_password_hide else R.string.add_password_show)
        }
        toggle = iconButton(R.drawable.ic_eye, getString(R.string.add_password_show), color(R.color.text_muted)) {
            shown = !shown
            refresh()
        }
        box.addView(toggle, FrameLayout.LayoutParams(dp(48), dp(48), Gravity.END or Gravity.CENTER_VERTICAL))
        refresh()
        return box
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
        for (f in listOf(address, password, pseudo, name)) f.isEnabled = !on
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
        val addr = ServerAddress.parse(address.text.toString())
        if (addr == null) return showError(address, getString(R.string.add_invalid_address))
        if (password.text.isEmpty()) return showError(password, getString(R.string.add_password_missing))
        val user = cleanUserName(pseudo.text.toString()) ?: return showError(pseudo, getString(R.string.pseudo_invalid))
        val deviceName = name.text.toString().trim()
        if (deviceName.isEmpty()) return showError(name, getString(R.string.add_device_name_missing))
        val pw = password.text.toString().toByteArray(Charsets.UTF_8)
        setBusy(true)
        Thread {
            var serverId = -1L
            val error: EnrollmentError? = try {
                val e = Enroller.enroll(this, addr, pw, deviceName, user)
                val sealed = KeyVault.seal(e.devicePrivateKey)
                e.devicePrivateKey.fill(0)
                serverId = Store.get(this).addServer(addr.http, e.serverName, e.deviceId, deviceName, sealed, e.serverPublicKey, user)
                null
            } catch (e: EnrollmentError) {
                e
            } catch (e: Exception) {
                EnrollmentError(getString(R.string.add_error, e.message ?: e.javaClass.simpleName))
            }
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                setBusy(false)
                if (error == null) done(serverId, user) else fail(error)
            }
        }.start()
    }

    private fun fail(e: EnrollmentError) {
        val text = e.message ?: getString(R.string.err_unknown)
        when (e.field) {
            EnrollField.ADDRESS -> showError(address, text)
            EnrollField.PASSWORD -> {
                password.text.clear()
                showError(password, text)
            }
            EnrollField.USERNAME -> showError(pseudo, text)
            EnrollField.NONE -> status.apply {
                this.text = text
                visibility = View.VISIBLE
            }
        }
    }

    /** Enrolled: the first username becomes the default, then straight to the server's groups. */
    private fun done(serverId: Long, user: String) {
        password.text.clear()
        if (Prefs.defaultUserName(this) == null) Prefs.setDefaultUserName(this, user)
        InboxService.start(this)
        startActivities(arrayOf(
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            Intent(this, ServerActivity::class.java).putExtra(ServerActivity.EXTRA_SERVER, serverId),
        ))
        finish()
    }
}
