package app.inbox

import android.app.Application
import app.inbox.alert.Alerter

class InboxApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Alerter.createChannels(this)
    }
}
