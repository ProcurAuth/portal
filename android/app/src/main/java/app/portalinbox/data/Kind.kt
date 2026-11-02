package app.portalinbox.data

import org.json.JSONObject

/**
 * The kind of a message. Protocol 4 has one kind: `alert`. A later protocol version can add
 * kinds. The app must not drop a message of a kind that it does not know. It stores the message
 * and shows it as a generic item.
 */
object Kind {
    const val ALERT = "alert"

    /** Stored for a frame whose kind is missing or is not a valid kind name. */
    const val UNKNOWN = "unknown"

    private val NAME = Regex("[a-z][a-z0-9_]{0,31}")

    /** The kind to store. A valid name stays as it is. Any other value becomes [UNKNOWN]. */
    fun normalize(raw: String?): String = if (raw != null && NAME.matches(raw)) raw else UNKNOWN

    /** True for a kind that this version of the app shows with its own words. */
    fun isKnown(kind: String): Boolean = kind == ALERT
}

/** The fields of an `alert` frame of the session. */
data class AlertFrame(
    val alertId: String,
    val kind: String,
    val group: String,
    val groupName: String,
    val title: String,
    val createdAtMs: Long,
) {
    companion object {
        /**
         * Reads the frame. A missing field of the delivery (id, group, title, time) throws, as before.
         * The field `kind` never throws: a value that the app does not know is kept for display.
         */
        fun parse(msg: JSONObject): AlertFrame = AlertFrame(
            alertId = msg.getString("alert_id"),
            kind = Kind.normalize(if (msg.isNull("kind")) null else (msg.opt("kind") as? String)),
            group = msg.getString("group"),
            groupName = msg.getString("group_name"),
            title = msg.getString("title"),
            createdAtMs = msg.getLong("created_at_ms"),
        )
    }
}
