package com.dispatch.radio.model

data class Agent(
    val slot: Int,
    val callsign: String,
    val tool: String,
    val status: String, // "busy", "idle", "empty"
    val task: String?
)

val NATO_CALLSIGNS = listOf(
    "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
    "India", "Juliet", "Kilo", "Lima", "Mike", "November", "Oscar", "Papa",
    "Quebec", "Romeo", "Sierra", "Tango", "Uniform", "Victor", "Whiskey",
    "X-ray", "Yankee", "Zulu"
)
