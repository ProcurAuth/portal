package app.portalinbox.portal

/**
 * The hint "Opened from: Chrome" of the confirmation screen. Android gives the referrer of an activity as a
 * URI: `android-app://<package>` when another app opened the link, or a web URL. The referrer is a hint and never proof: an app
 * can set it. Pure Kotlin.
 */
object ReferrerHint {
    private val PACKAGE = Regex("[A-Za-z][A-Za-z0-9_]*(\\.[A-Za-z][A-Za-z0-9_]*)+")
    private val HOST = Regex("[A-Za-z0-9]([A-Za-z0-9.-]{0,251}[A-Za-z0-9])?")

    /**
     * The text to show for [referrer]: the package name, or the host of a web page. Null when Android gave nothing, or when the
     * text has a shape that this class does not trust (the screen then says "unknown"). Nothing else of the URI is kept.
     */
    fun of(referrer: String?): String? {
        if (referrer.isNullOrBlank()) return null
        val scheme = referrer.substringBefore("://", "")
        val rest = referrer.substringAfter("://", "")
        if (rest.isEmpty()) return null
        return when (scheme.lowercase()) {
            "android-app" -> rest.substringBefore('/').takeIf { PACKAGE.matches(it) }
            "http", "https" -> rest.substringBefore('/').substringBefore('?').substringBefore('#').substringBefore(':')
                .takeIf { HOST.matches(it) }?.lowercase()
            else -> null
        }
    }
}
