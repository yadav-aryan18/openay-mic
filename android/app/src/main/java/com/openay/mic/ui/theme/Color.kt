package com.openay.mic.ui.theme

import androidx.compose.ui.graphics.Color

// Binding palette — design.md "studio rack at night". Warm grays only.
val Ink = Color(0xFF14120F)        // app background
val Panel = Color(0xFF1E1B16)      // raised surfaces/cards
val PanelRaised = Color(0xFF262219) // subtle top-lit panel shading (<=6% lightness)
val Line = Color(0xFF34302A)       // hairlines, segment tracks
val Cream = Color(0xFFEFE6D4)      // primary text
val Amber = Color(0xFFFFB454)      // active/live accents
val Tally = Color(0xFFE5484D)      // ON AIR lamp + clip zone only
val Dim = Color(0xFF8D8477)        // secondary/disabled text

// Derived containers (palette mixes for Material slots / tinted panels).
val AmberContainer = Color(0xFF4B3A22)   // amber ~20% over panel
val TallyContainer = Color(0xFF331A18)   // tally ~15% over ink
val TallySoft = Color(0xFFF5C4C6)        // readable text on TallyContainer