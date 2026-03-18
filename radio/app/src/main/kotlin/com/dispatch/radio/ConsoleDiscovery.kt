package com.dispatch.radio

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Handler
import android.os.Looper

/**
 * Discovers Dispatch Console instances on the LAN via mDNS/NSD (dispatch-ct2.1).
 *
 * The console advertises `_dispatch._tcp.`. This class wraps Android's NsdManager
 * to discover and resolve those services, returning host and port to the caller.
 */
class ConsoleDiscovery(context: Context) {

    data class Console(val host: String, val port: Int, val name: String)

    interface Listener {
        fun onConsoleFound(console: Console)
        fun onDiscoveryStopped()
    }

    private val nsdManager = context.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val mainHandler = Handler(Looper.getMainLooper())

    @Volatile private var discovering = false
    private var listener: Listener? = null

    fun startDiscovery(listener: Listener) {
        if (discovering) return
        this.listener = listener
        discovering = true
        nsdManager.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, discoveryListener)
    }

    fun stopDiscovery() {
        if (!discovering) return
        discovering = false
        try {
            nsdManager.stopServiceDiscovery(discoveryListener)
        } catch (_: IllegalArgumentException) {
            // Already stopped
        }
    }

    private val discoveryListener = object : NsdManager.DiscoveryListener {
        override fun onDiscoveryStarted(serviceType: String) {}

        override fun onServiceFound(serviceInfo: NsdServiceInfo) {
            nsdManager.resolveService(serviceInfo, object : NsdManager.ResolveListener {
                override fun onResolveFailed(si: NsdServiceInfo, errorCode: Int) {}

                override fun onServiceResolved(si: NsdServiceInfo) {
                    val host = si.host?.hostAddress ?: return
                    val console = Console(host, si.port, si.serviceName)
                    mainHandler.post { listener?.onConsoleFound(console) }
                }
            })
        }

        override fun onServiceLost(serviceInfo: NsdServiceInfo) {}

        override fun onDiscoveryStopped(serviceType: String) {
            mainHandler.post { listener?.onDiscoveryStopped() }
        }

        override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
            discovering = false
        }

        override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
            discovering = false
        }
    }

    companion object {
        private const val SERVICE_TYPE = "_dispatch._tcp."
    }
}
