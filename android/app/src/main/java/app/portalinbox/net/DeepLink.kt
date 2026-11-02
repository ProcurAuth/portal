package app.portalinbox.net

/**
 * The deep link of an enrollment code: `portal-inbox://enroll?s=<address>&k=<server key>&c=<secret>`.
 * The manifest declares the same scheme and host. A unit test compares them.
 * The core reads the query. The app only decides whether a link is for the enrollment screen.
 */
object DeepLink {
    const val SCHEME = "portal-inbox"
    const val HOST = "enroll"

    /** True when the scheme and the host are exactly those of an enrollment link. */
    fun isEnroll(scheme: String?, host: String?): Boolean = scheme == SCHEME && host == HOST
}
