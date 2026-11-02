package app.portalinbox.net

import java.net.URI

/** A normalized server address. `http` is the base URL, used to derive WebSocket URLs. */
data class ServerAddress(val http: String) {
    val secure: Boolean get() = http.startsWith("https://")

    fun ws(path: String): String =
        (if (secure) "wss://" else "ws://") + http.substringAfter("://") + path

    companion object {
        /**
         * Accepts `ip:port`, `host`, `host:port`, `http(s)://host[:port][/prefix]` and `ws(s)://...`.
         * Returns null if the input is not a usable address.
         */
        fun parse(input: String): ServerAddress? {
            var s = input.trim()
            if (s.isEmpty() || s.any { it.isWhitespace() }) return null
            if (!s.contains("://")) s = "http://$s"
            val uri = try {
                URI(s)
            } catch (e: Exception) {
                return null
            }
            val scheme = uri.scheme?.lowercase() ?: return null
            val secure = when (scheme) {
                "http", "ws" -> false
                "https", "wss" -> true
                else -> return null
            }
            val host = uri.host ?: return null
            if (uri.rawQuery != null || uri.rawFragment != null || uri.rawUserInfo != null) return null
            val port = if (uri.port == -1) "" else ":${uri.port}"
            val path = (uri.rawPath ?: "").trimEnd('/')
            return ServerAddress((if (secure) "https://" else "http://") + host + port + path)
        }
    }
}
