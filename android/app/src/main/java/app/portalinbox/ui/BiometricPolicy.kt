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

    /**
     * The title of the prompt: "Approve on Production". The prompt is system UI, so it cannot show the color of the portal.
     * The nickname is the cue. The nickname is local: the portal never sets it.
     */
    fun title(nickname: String): String = "Approve on " + VisibleText.escapeLine(nickname)

    /** The subtitle of the prompt: the code, the service and the short hash. It never shows the command. */
    fun subtitle(code: String, service: String, shortEffect: String?): String =
        listOfNotNull(VisibleText.escapeLine(code), VisibleText.escapeLine(service), shortEffect?.let { "sha256:$it" }).joinToString(" · ")

    /** The title of the prompt before an answer to a question: "Send answer on Production". */
    fun answerTitle(nickname: String): String = "Send answer on " + VisibleText.escapeLine(nickname)

    /** The subtitle of the prompt: the title of the question, on one line. It never shows the answer. */
    fun answerSubtitle(questionTitle: String): String = QuestionHistory.promptTitle(questionTitle)
}
