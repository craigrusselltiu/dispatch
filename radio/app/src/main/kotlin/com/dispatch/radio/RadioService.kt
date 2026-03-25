package com.dispatch.radio

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.Binder
import android.os.IBinder

/**
 * Foreground service that keeps the WebSocket connection alive when the app
 * is minimized or the screen is off. Without this, Android kills the process
 * and the radio silently disconnects.
 *
 * The service owns the [RadioWebSocketClient]. MainActivity binds to it and
 * sets a [listener] to receive connection and message callbacks.
 */
class RadioService : Service() {

    inner class LocalBinder : Binder() {
        fun getService() = this@RadioService
    }

    private val binder = LocalBinder()
    private var wsClient: RadioWebSocketClient? = null
    private var cachedPendingIntent: PendingIntent? = null

    /** Activity sets this to receive WebSocket callbacks. */
    var listener: RadioWebSocketClient.Listener? = null

    /** Whether the WebSocket is currently connected. */
    var isConnected = false
        private set

    override fun onBind(intent: Intent): IBinder = binder

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        startForeground(NOTIFICATION_ID, buildNotification("Connecting..."))
    }

    /**
     * Connect (or reconnect) the WebSocket with the given settings.
     * Disconnects any existing connection first.
     */
    fun connectWebSocket(host: String, port: Int, psk: String, certFingerprint: String?) {
        wsClient?.disconnect()
        wsClient = RadioWebSocketClient(
            host = host,
            port = port,
            psk = psk,
            listener = object : RadioWebSocketClient.Listener {
                override fun onConnected() {
                    isConnected = true
                    updateNotification("Connected")
                    listener?.onConnected()
                }
                override fun onDisconnected() {
                    isConnected = false
                    updateNotification("Disconnected")
                    listener?.onDisconnected()
                }
                override fun onMessage(text: String) {
                    listener?.onMessage(text)
                }
            },
            certFingerprint = certFingerprint,
        )
        wsClient?.connect()
    }

    fun send(text: String): Boolean = wsClient?.send(text) ?: false

    override fun onDestroy() {
        wsClient?.disconnect()
        super.onDestroy()
    }

    private fun createNotificationChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID, "Dispatch Radio", NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Keeps the console connection alive"
        }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun ensurePendingIntent(): PendingIntent {
        return cachedPendingIntent ?: run {
            val intent = Intent(this, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
            }
            PendingIntent.getActivity(this, 0, intent, PendingIntent.FLAG_IMMUTABLE).also {
                cachedPendingIntent = it
            }
        }
    }

    private fun buildNotification(status: String): Notification {
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("Dispatch Radio")
            .setContentText(status)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentIntent(ensurePendingIntent())
            .setOngoing(true)
            .build()
    }

    private fun updateNotification(status: String) {
        getSystemService(NotificationManager::class.java)
            .notify(NOTIFICATION_ID, buildNotification(status))
    }

    companion object {
        private const val CHANNEL_ID = "dispatch_radio"
        private const val NOTIFICATION_ID = 1
    }
}
