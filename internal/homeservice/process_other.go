//go:build !darwin && !linux

package homeservice

import "errors"

func readProcess(pid int) (process, error) {
	return process{}, errors.New("Home services require macOS or Linux")
}
func connectors() ([]process, error) { return nil, errors.New("Home services require macOS or Linux") }
