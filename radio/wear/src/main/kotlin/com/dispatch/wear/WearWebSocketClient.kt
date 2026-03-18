package com.dispatch.wear

import android.os.Handler
import android.os.Looper
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import java.security.MessageDigest
import java.security.cert.X509Certificate
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLContext
import javax.net.ssl.X509TrustManager

/**
 * WebSocket client for the Dispatch Wear companion.
 * Same protocol as the phone radio -- connects to wss://host:port/?psk=<key>.
 *
 * TLS: When a cert fingerprint is provided, pins to that specific certificate.
 * When no fingerprint is provided, trusts all certificates (encrypted but no
 * pinning -- the PSK still authenticates the connection).
 */
class WearWebSocketClient(
    private val host: String,
    private val port: Int,
    private val psk: String,
    private val listener: Listener,
    private val certFingerprint: String? = null,
) {
    interface Listener {
        fun onConnected()
        fun onMessage(text: String)
        fun onDisconnected()
    }

    private val httpClient: OkHttpClient = buildClient(certFingerprint)

    private val mainHandler = Handler(Looper.getMainLooper())

    @Volatile private var webSocket: WebSocket? = null
    @Volatile private var connected = false
    @Volatile private var stopped = false

    private var reconnectDelay = INITIAL_DELAY_MS

    fun connect() {
        stopped = false
        openConnection()
    }

    fun disconnect() {
        stopped = true
        reconnectDelay = INITIAL_DELAY_MS
        mainHandler.removeCallbacksAndMessages(null)
        webSocket?.close(CLOSE_NORMAL, "disconnect")
        webSocket = null
        connected = false
    }

    fun send(text: String): Boolean {
        val ws = webSocket ?: return false
        return ws.send(text)
    }

    private fun openConnection() {
        val url = "wss://$host:$port/?psk=$psk"
        val request = Request.Builder().url(url).build()
        webSocket = httpClient.newWebSocket(request, socketListener)
    }

    private val socketListener = object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            connected = true
            reconnectDelay = INITIAL_DELAY_MS
            webSocket.send(MSG_LIST_AGENTS)
            mainHandler.post { listener.onConnected() }
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            mainHandler.post { listener.onMessage(text) }
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            mainHandler.post { listener.onMessage(bytes.utf8()) }
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            handleDisconnect()
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            handleDisconnect()
        }
    }

    private fun handleDisconnect() {
        val wasConnected = connected
        connected = false
        webSocket = null

        if (wasConnected) {
            mainHandler.post { listener.onDisconnected() }
        }

        if (!stopped) {
            scheduleReconnect()
        }
    }

    private fun scheduleReconnect() {
        mainHandler.postDelayed({
            if (!stopped) {
                openConnection()
            }
        }, reconnectDelay)

        reconnectDelay = (reconnectDelay * 2).coerceAtMost(MAX_DELAY_MS)
    }

    companion object {
        private const val PING_INTERVAL_SECONDS = 15L
        private const val INITIAL_DELAY_MS = 1_000L
        private const val MAX_DELAY_MS = 30_000L
        private const val CLOSE_NORMAL = 1000
        private const val MSG_LIST_AGENTS = """{"type":"list_agents"}"""

        /**
         * Build an OkHttpClient that trusts the console's self-signed certificate.
         * If [fingerprint] is non-null, only certs matching that SHA-256 fingerprint
         * are accepted. Otherwise, all certs are trusted (encryption without pinning).
         */
        fun buildClient(fingerprint: String?): OkHttpClient {
            val trustManager = object : X509TrustManager {
                override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {}
                override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
                    if (fingerprint == null || chain.isNullOrEmpty()) return
                    val cert = chain[0]
                    val sha256 = MessageDigest.getInstance("SHA-256").digest(cert.encoded)
                    val hex = sha256.joinToString("") { "%02x".format(it) }
                    if (hex != fingerprint) {
                        throw java.security.cert.CertificateException(
                            "Certificate fingerprint mismatch: expected $fingerprint, got $hex"
                        )
                    }
                }
                override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
            }

            val sslContext = SSLContext.getInstance("TLS")
            sslContext.init(null, arrayOf(trustManager), null)

            return OkHttpClient.Builder()
                .pingInterval(PING_INTERVAL_SECONDS, TimeUnit.SECONDS)
                .sslSocketFactory(sslContext.socketFactory, trustManager)
                .hostnameVerifier { _, _ -> true }
                .build()
        }
    }
}
