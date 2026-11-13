package app.portalinbox.core

/** One field of a resolved view: the label of the template and the value of `op` that its pointer names. */
data class ResolvedField(val label: String, val value: String)

/**
 * The view of an approval with the values of `op`. `body` is the command: the text that will run. An empty `title` means
 * that the view has none, and an empty `body` means that the request has no command text (ProcurAuth 17.9).
 * [ShownEffect] puts the name of the action in an empty title.
 */
data class ResolvedView(
    val title: String,
    val fields: List<ResolvedField>,
    val body: String,
    val bodyLabel: String?,
)

sealed interface EffectResult {
    data class Hash(val effect: String) : EffectResult

    /** `code` is `invalid_op` or `invalid_view`. */
    data class Failure(val code: String, val message: String) : EffectResult
}

sealed interface ViewResult {
    data class Resolved(val view: ResolvedView) : ViewResult

    data class Failure(val code: String, val message: String) : ViewResult
}

/**
 * The effect hash of an operation and the view that shows it (ProcurAuth 17.3). The real
 * implementation is [NativeEffectHasher]: it runs the Rust code of `inbox-core` through JNI, so
 * the rule exists once. JVM tests inject a fake, because the JNI library does not load there.
 * The Rust tests and the vectors prove the hash itself.
 */
interface EffectHasher {
    /** The effect of the strings and of `op`, given as JSON text. */
    fun effect(service: String, action: String, resource: String, opJson: String): EffectResult

    /** The view with the values of `op`. Both arguments are JSON text. */
    fun view(opJson: String, viewJson: String): ViewResult
}
