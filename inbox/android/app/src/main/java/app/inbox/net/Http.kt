package app.inbox.net

import okhttp3.OkHttpClient
import java.util.concurrent.TimeUnit

object Http {
    /** WebSockets only: liveness is handled by Inbox pings, so no read timeout here. */
    val client: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(15, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .writeTimeout(15, TimeUnit.SECONDS)
        .retryOnConnectionFailure(true)
        .build()
}
