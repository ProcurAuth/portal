package app.portalinbox

import android.app.Application
import app.portalinbox.alert.Alerter
import app.portalinbox.ui.ApprovalActivity
import app.portalinbox.ui.ThreadActivity

class InboxApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Alerter.createChannels(this)
        // "Ask the agent" of the approval screen opens the discussion.
        ApprovalActivity.Nav.ask = { context, serverId, requestId -> context.startActivity(ThreadActivity.intent(context, serverId, requestId)) }
    }
}
