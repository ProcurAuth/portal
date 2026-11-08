package app.portalinbox.ui

/**
 * Which authenticators the local prompt accepts before an Approve. Pure Kotlin: the Activity maps the
 * answer to the `BiometricManager.Authenticators` flags. The prompt is a local gate that the server cannot check.
 */
object BiometricPolicy {
    enum class Allowed {
        /** A strong biometric or the credential of the device (PIN, pattern, password). API 30 and above, and API 26 and 27. */
        STRONG_OR_CREDENTIAL,

        /**
         * A weak biometric or the credential. API 28 and 29: the platform does not take a strong biometric together
         * with the credential on these versions.
         */
        WEAK_OR_CREDENTIAL,
    }

    fun allowed(sdkInt: Int): Allowed =
        if (sdkInt in 28..29) Allowed.WEAK_OR_CREDENTIAL else Allowed.STRONG_OR_CREDENTIAL

    /** The title of the prompt: it shows the code only, never the command. */
    fun title(code: String): String = "Approve $code"

    /** The subtitle of the prompt: the service and the short hash. */
    fun subtitle(service: String, shortEffect: String?): String =
        listOfNotNull(VisibleText.escapeLine(service), shortEffect?.let { "sha256:$it" }).joinToString(" · ")
}
