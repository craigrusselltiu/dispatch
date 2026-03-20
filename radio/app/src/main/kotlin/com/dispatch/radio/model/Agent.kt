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

/** Distinct color for each NATO callsign, readable on a dark background. */
private val CALLSIGN_COLORS = mapOf(
    "Alpha"    to 0xFF00FFFF.toInt(), // Cyan
    "Bravo"    to 0xFFFF9900.toInt(), // Orange
    "Charlie"  to 0xFFFFEE00.toInt(), // Yellow
    "Delta"    to 0xFFFF66AA.toInt(), // Pink
    "Echo"     to 0xFF88FF44.toInt(), // Lime
    "Foxtrot"  to 0xFFFF6644.toInt(), // Coral
    "Golf"     to 0xFF6699FF.toInt(), // Cornflower
    "Hotel"    to 0xFF00FFAA.toInt(), // Mint
    "India"    to 0xFFFFBB66.toInt(), // Peach
    "Juliet"   to 0xFFBB77FF.toInt(), // Lavender
    "Kilo"     to 0xFF44FFAA.toInt(), // Spring Green
    "Lima"     to 0xFF77CCFF.toInt(), // Light Blue
    "Mike"     to 0xFFFFDD77.toInt(), // Light Gold
    "November" to 0xFF77FF99.toInt(), // Pale Green
    "Oscar"    to 0xFFFF7744.toInt(), // Salmon
    "Papa"     to 0xFFDD88FF.toInt(), // Plum
    "Quebec"   to 0xFF88DDFF.toInt(), // Powder Blue
    "Romeo"    to 0xFFFF4488.toInt(), // Deep Pink
    "Sierra"   to 0xFF66FFCC.toInt(), // Aquamarine
    "Tango"    to 0xFFFFAA44.toInt(), // Dark Gold
    "Uniform"  to 0xFFCCCCCC.toInt(), // Light Gray
    "Victor"   to 0xFFEE77EE.toInt(), // Violet
    "Whiskey"  to 0xFFDDCC88.toInt(), // Wheat
    "X-ray"    to 0xFF44DDDD.toInt(), // Dark Cyan
    "Yankee"   to 0xFFDDDD66.toInt(), // Olive Yellow
    "Zulu"     to 0xFFCC9988.toInt(), // Rosy Brown
)

/** Returns the distinct color int for a callsign, defaulting to cyan. */
fun callsignColor(callsign: String): Int =
    CALLSIGN_COLORS[callsign] ?: 0xFF00FFFF.toInt()
