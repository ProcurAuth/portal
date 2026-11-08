package app.portalinbox.data

import app.portalinbox.core.EffectHasher
import app.portalinbox.core.EffectResult
import app.portalinbox.core.ResolvedView
import app.portalinbox.core.ViewResult
import app.portalinbox.proto.ApprovalBody

/**
 * The check that the phone runs on each approval. The phone computes the effect again from the
 * `op` that it received. It shows the command from the same `op`. The effect that the owner
 * approves is the hash of what the screen shows.
 */
object ShownEffect {
    sealed interface Verdict {
        /** The computed hash is the `effect` of the frame and every pointer of the view names a value. */
        data class Valid(val effect: String, val view: ResolvedView) : Verdict

        data class Invalid(val validity: Validity) : Verdict
    }

    fun verify(approval: ApprovalBody, hasher: EffectHasher): Verdict {
        val opJson = approval.opJson
        val effect = when (val r = hasher.effect(approval.service, approval.action, approval.resource, opJson)) {
            is EffectResult.Hash -> r.effect
            is EffectResult.Failure -> return Verdict.Invalid(Validity.INVALID_OP)
        }
        if (effect != approval.effect) return Verdict.Invalid(Validity.EFFECT_MISMATCH)
        return when (val v = hasher.view(opJson, approval.viewJson)) {
            is ViewResult.Resolved -> Verdict.Valid(effect, v.view)
            is ViewResult.Failure -> Verdict.Invalid(if (v.code == "invalid_op") Validity.INVALID_OP else Validity.INVALID_VIEW)
        }
    }

    /** The short form of an effect for the screen: the first 8 hexadecimal digits, after `sha256:`. */
    fun short(effect: String): String? = if (effect.startsWith("sha256:") && effect.length == 71) effect.substring(7, 15) else null
}
