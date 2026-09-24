package main

import (
	"fmt"
	"runtime"
	"unicode"
)

func emit(name string, allowed func(rune) bool) {
	fmt.Printf("pub(super) const %s: &[(u32, u32)] = &[\n", name)
	start, last := -1, -1
	flush := func() {
		if start >= 0 {
			fmt.Printf("    (0x%X, 0x%X),\n", start, last)
		}
	}
	for value := 0; value <= unicode.MaxRune; value++ {
		if allowed(rune(value)) {
			if start < 0 {
				start, last = value, value
			} else if value == last+1 {
				last = value
			} else {
				flush()
				start, last = value, value
			}
		}
	}
	flush()
	fmt.Println("];")
}
func main() {
	if unicode.Version != "15.0.0" {
		panic("workspace slug compatibility requires Unicode 15.0.0")
	}
	fmt.Printf("// Generated from Go %s unicode.Version=%s. Do not edit ranges by hand.\n", runtime.Version(), unicode.Version)
	fmt.Println("// These tables preserve Go workspaceSlug/prepareCreate Unicode categories without a Go runtime.")
	emit("LETTER_OR_NUMBER", func(r rune) bool { return unicode.IsLetter(r) || unicode.IsNumber(r) })
	emit("SPACE", unicode.IsSpace)
	emit("CONTROL", unicode.IsControl)
}
