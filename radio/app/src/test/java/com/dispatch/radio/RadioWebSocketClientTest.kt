package com.dispatch.radio

import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class RadioWebSocketClientTest {

    private lateinit var server: MockWebServer

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
    }

    @After
    fun tearDown() {
        server.shutdown()
    }

    @Test
    fun `PSK is included in connection URL as query param`() {
        server.enqueue(MockResponse().withWebSocketUpgrade(NoOpWebSocketListener()))

        val latch = CountDownLatch(1)
        val client = RadioWebSocketClient(
            host = server.hostName,
            port = server.port,
            psk = "test-secret-key",
            listener = object : RadioWebSocketClient.Listener {
                override fun onConnected() { latch.countDown() }
                override fun onMessage(text: String) {}
                override fun onDisconnected() {}
                override fun onReconnectGaveUp() {}
            }
        )

        client.connect()
        latch.await(5, TimeUnit.SECONDS)

        val request: RecordedRequest = server.takeRequest(1, TimeUnit.SECONDS)!!
        assertTrue(
            "URL should contain psk query param",
            request.path!!.contains("psk=test-secret-key")
        )

        client.disconnect()
    }

    @Test
    fun `list_agents is sent on connect`() {
        val receivedMessages = mutableListOf<String>()
        val latch = CountDownLatch(1)
        val serverListener = object : NoOpWebSocketListener() {
            override fun onMessage(webSocket: okhttp3.WebSocket, text: String) {
                receivedMessages.add(text)
                latch.countDown()
            }
        }
        server.enqueue(MockResponse().withWebSocketUpgrade(serverListener))

        val client = RadioWebSocketClient(
            host = server.hostName,
            port = server.port,
            psk = "key",
            listener = object : RadioWebSocketClient.Listener {
                override fun onConnected() {}
                override fun onMessage(text: String) {}
                override fun onDisconnected() {}
                override fun onReconnectGaveUp() {}
            }
        )

        client.connect()
        latch.await(5, TimeUnit.SECONDS)

        assertEquals(1, receivedMessages.size)
        assertEquals("""{"type":"list_agents"}""", receivedMessages[0])

        client.disconnect()
    }

    @Test
    fun `onConnected callback fires after successful handshake`() {
        server.enqueue(MockResponse().withWebSocketUpgrade(NoOpWebSocketListener()))

        val latch = CountDownLatch(1)
        val client = RadioWebSocketClient(
            host = server.hostName,
            port = server.port,
            psk = "key",
            listener = object : RadioWebSocketClient.Listener {
                override fun onConnected() { latch.countDown() }
                override fun onMessage(text: String) {}
                override fun onDisconnected() {}
                override fun onReconnectGaveUp() {}
            }
        )

        client.connect()
        assertTrue("onConnected should fire within 5s", latch.await(5, TimeUnit.SECONDS))

        client.disconnect()
    }

    @Test
    fun `send returns false when not connected`() {
        val client = RadioWebSocketClient(
            host = "localhost",
            port = 9999,
            psk = "key",
            listener = object : RadioWebSocketClient.Listener {
                override fun onConnected() {}
                override fun onMessage(text: String) {}
                override fun onDisconnected() {}
                override fun onReconnectGaveUp() {}
            }
        )

        val result = client.send("""{"type":"test"}""")
        assertTrue("send should return false when not connected", !result)
    }

    @Test
    fun `auto-reconnect attempts after server closes connection`() {
        // First connection succeeds, then disconnects
        server.enqueue(MockResponse().withWebSocketUpgrade(object : NoOpWebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                webSocket.close(1000, "goodbye")
            }
        }))
        // Second connection attempt
        server.enqueue(MockResponse().withWebSocketUpgrade(NoOpWebSocketListener()))

        val connectCount = java.util.concurrent.atomic.AtomicInteger(0)
        val secondConnectLatch = CountDownLatch(2)

        val client = RadioWebSocketClient(
            host = server.hostName,
            port = server.port,
            psk = "key",
            listener = object : RadioWebSocketClient.Listener {
                override fun onConnected() {
                    connectCount.incrementAndGet()
                    secondConnectLatch.countDown()
                }
                override fun onMessage(text: String) {}
                override fun onDisconnected() {}
                override fun onReconnectGaveUp() {}
            }
        )

        client.connect()
        // Wait up to 5s for reconnect (initial delay is 1s)
        secondConnectLatch.await(5, TimeUnit.SECONDS)

        assertTrue("Should have connected at least twice", connectCount.get() >= 2)

        client.disconnect()
    }
}

private open class NoOpWebSocketListener : okhttp3.WebSocketListener() {
    override fun onOpen(webSocket: WebSocket, response: Response) {}
    override fun onMessage(webSocket: WebSocket, text: String) {}
    override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
        webSocket.close(1000, null)
    }
}
