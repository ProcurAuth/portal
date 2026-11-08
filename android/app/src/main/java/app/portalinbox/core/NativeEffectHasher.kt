package app.portalinbox.core

import org.json.JSONObject

/** The [EffectHasher] that calls `inbox-core` through [Native]. */
object NativeEffectHasher : EffectHasher {
    override fun effect(service: String, action: String, resource: String, opJson: String): EffectResult {
        val r = JSONObject(Native.effectCompute(service, action, resource, opJson))
        r.optJSONObject("error")?.let { return EffectResult.Failure(it.optString("code"), it.optString("message")) }
        return EffectResult.Hash(r.getString("effect"))
    }

    override fun view(opJson: String, viewJson: String): ViewResult {
        val r = JSONObject(Native.viewResolve(opJson, viewJson))
        r.optJSONObject("error")?.let { return ViewResult.Failure(it.optString("code"), it.optString("message")) }
        val v = r.getJSONObject("view")
        val fields = v.getJSONArray("fields")
        return ViewResult.Resolved(
            ResolvedView(
                title = v.getString("title"),
                fields = (0 until fields.length()).map {
                    val f = fields.getJSONObject(it)
                    ResolvedField(f.getString("label"), f.getString("value"))
                },
                body = v.getString("body"),
                bodyLabel = if (v.has("body_label") && !v.isNull("body_label")) v.getString("body_label") else null,
            ),
        )
    }
}
