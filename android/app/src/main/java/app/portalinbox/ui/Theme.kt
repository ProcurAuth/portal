package app.portalinbox.ui

import android.app.Activity
import android.content.Context
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import android.view.View
import android.view.WindowInsets
import android.view.WindowInsetsController

/** User display preferences. */
object Prefs {
    private const val FILE = "inbox_ui"
    const val DEFAULT_LIMIT = 10
    const val DEFAULT_WINDOW_MS = 24L * 3_600_000
    val LIMIT_RANGE = 1..50
    val WINDOWS_MS = listOf(1L, 6L, 12L, 24L, 48L, 168L).map { it * 3_600_000 }

    enum class ThemeMode { SYSTEM, LIGHT, DARK }

    private fun prefs(c: Context) = c.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    fun limit(c: Context): Int =
        prefs(c).getInt("alerts_shown", DEFAULT_LIMIT).coerceIn(LIMIT_RANGE.first, LIMIT_RANGE.last)

    fun setLimit(c: Context, v: Int) =
        prefs(c).edit().putInt("alerts_shown", v.coerceIn(LIMIT_RANGE.first, LIMIT_RANGE.last)).apply()

    /** Whether the home list also shows alerts already read. Off by default. */
    fun showRead(c: Context): Boolean = prefs(c).getBoolean("show_read", false)
    fun setShowRead(c: Context, v: Boolean) = prefs(c).edit().putBoolean("show_read", v).apply()

    fun windowMs(c: Context): Long = prefs(c).getLong("display_window_ms", DEFAULT_WINDOW_MS)
    fun setWindowMs(c: Context, v: Long) = prefs(c).edit().putLong("display_window_ms", v).apply()

    fun theme(c: Context): ThemeMode =
        runCatching { ThemeMode.valueOf(prefs(c).getString("theme", null) ?: "SYSTEM") }.getOrDefault(ThemeMode.SYSTEM)

    fun setTheme(c: Context, v: ThemeMode) = prefs(c).edit().putString("theme", v.name).apply()

    /** How long the phone keeps the local copy of a finished request, in days (an option of `LocalRetention`). */
    fun localRetentionDays(c: Context): Int =
        app.portalinbox.data.LocalRetention.daysOrDefault(prefs(c).getInt("local_retention_days", app.portalinbox.data.LocalRetention.DEFAULT_DAYS))

    fun setLocalRetentionDays(c: Context, days: Int) {
        if (app.portalinbox.data.LocalRetention.isOption(days)) prefs(c).edit().putInt("local_retention_days", days).apply()
    }

    /** The groups of "To answer" that the owner folded. Local to the phone: see [AgentCollapse]. */
    fun agentCollapse(c: Context): AgentCollapse = AgentCollapse.decode(prefs(c).getString("agent_folded", null))

    fun setAgentCollapse(c: Context, v: AgentCollapse) = prefs(c).edit().putString("agent_folded", v.encode()).apply()

    /** True once after an update removed servers of an older protocol (see `Store.onUpgrade`). */
    fun takeEnrollAgain(c: Context): Boolean {
        val set = prefs(c).getBoolean(app.portalinbox.data.Store.PREF_ENROLL_AGAIN, false)
        if (set) prefs(c).edit().remove(app.portalinbox.data.Store.PREF_ENROLL_AGAIN).apply()
        return set
    }
}

/**
 * Base for the app screens: applies the chosen theme (system by default), draws
 * edge to edge over the coloured backdrop and pads the content by the system
 * insets.
 */
abstract class BaseActivity : Activity() {
    override fun attachBaseContext(newBase: Context) {
        super.attachBaseContext(themedContext(newBase))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        styleSystemBars(this)
    }

    val isNight: Boolean get() = isNightMode(this)

    /** Bottom snackbar over this screen (see [android.widget.FrameLayout.snackbar]). */
    fun snackbar(text: String, action: String? = null, onAction: () -> Unit = {}) =
        findViewById<android.widget.FrameLayout>(android.R.id.content).snackbar(text, action, onAction = onAction)

    /** Sets the content over the backdrop and keeps it clear of the system bars. */
    fun setScreen(root: View) {
        setContentView(withBackdrop(root))
        applyInsets(root)
    }
}

/**
 * The same base for a screen that needs a `FragmentActivity`: the biometric prompt of the approval screen. The look
 * and the helpers are those of [BaseActivity].
 */
abstract class BaseFragmentActivity : androidx.fragment.app.FragmentActivity() {
    override fun attachBaseContext(newBase: Context) {
        super.attachBaseContext(themedContext(newBase))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        styleSystemBars(this)
    }

    val isNight: Boolean get() = isNightMode(this)

    fun snackbar(text: String, action: String? = null, onAction: () -> Unit = {}) =
        findViewById<android.widget.FrameLayout>(android.R.id.content).snackbar(text, action, onAction = onAction)

    fun setScreen(root: View) {
        setContentView(withBackdrop(root))
        applyInsets(root)
    }
}

/** The context with the theme that the owner chose (light, dark or the system one). */
fun themedContext(newBase: Context): Context {
    val mode = Prefs.theme(newBase)
    if (mode == Prefs.ThemeMode.SYSTEM) return newBase
    val config = Configuration(newBase.resources.configuration)
    val night = if (mode == Prefs.ThemeMode.DARK) Configuration.UI_MODE_NIGHT_YES else Configuration.UI_MODE_NIGHT_NO
    config.uiMode = (config.uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or night
    return newBase.createConfigurationContext(config)
}

fun isNightMode(activity: Activity): Boolean =
    (activity.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES

/** Transparent system bars over the coloured backdrop (edge to edge on every version). */
@Suppress("DEPRECATION")
fun styleSystemBars(activity: Activity) {
    val window = activity.window
    window.statusBarColor = 0x00000000
    window.navigationBarColor = 0x00000000
    if (Build.VERSION.SDK_INT >= 29) window.isNavigationBarContrastEnforced = false
    val light = !isNightMode(activity)
    var flags = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or
        View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
    if (Build.VERSION.SDK_INT < 30) {
        if (light) flags = flags or View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR
        if (light && Build.VERSION.SDK_INT >= 27) flags = flags or View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR
    }
    window.decorView.systemUiVisibility = flags
    if (Build.VERSION.SDK_INT >= 30) {
        val mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or
            WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS
        window.decorView.post {
            window.insetsController?.setSystemBarsAppearance(if (light) mask else 0, mask)
        }
    }
}

/** Pads `view` by the system bar insets it receives (zero when not edge-to-edge). */
@Suppress("DEPRECATION")
fun applyInsets(view: View) {
    val base = intArrayOf(view.paddingLeft, view.paddingTop, view.paddingRight, view.paddingBottom)
    view.setOnApplyWindowInsetsListener { v, insets ->
        val (l, t, r, b) = if (Build.VERSION.SDK_INT >= 30) {
            // The keyboard inset too, so fields being edited stay above it.
            val i = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            val ime = insets.getInsets(WindowInsets.Type.ime())
            listOf(i.left, i.top, i.right, maxOf(i.bottom, ime.bottom))
        } else {
            listOf(insets.systemWindowInsetLeft, insets.systemWindowInsetTop, insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
        }
        v.setPadding(base[0] + l, base[1] + t, base[2] + r, base[3] + b)
        insets
    }
    view.requestApplyInsets()
}
