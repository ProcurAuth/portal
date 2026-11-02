package app.portalinbox

import android.app.Application
import app.portalinbox.alert.Alerter

class InboxApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Alerter.createChannels(this)
    }
}
