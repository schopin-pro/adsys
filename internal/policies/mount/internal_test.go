package mount

import (
	"flag"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
	"github.com/ubuntu/adsys/internal/testutils"
)

var Update bool

func TestParseEntryValues(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		entry string
	}{
		// Single entry cases.
		"parse values from entry with one value":        {entry: "entry with one value"},
		"parse values from entry with multiple values":  {entry: "entry with multiple values"},
		"parse values from entry with repeatead values": {entry: "entry with repeatead values"},

		// Badly formatted entries.
		"parse values trimming whitespaces":           {entry: "entry with spaces"},
		"parse values trimming sequential linebreaks": {entry: "entry with multiple linebreaks"},

		// Special cases.
		"parse values from entry with anonymous tags": {entry: "entry with anonymous tags"},
		"returns empty slice if the entry is empty":   {entry: "entry with no value"},
	}

	for name, tc := range tests {
		tc := tc
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			got := parseEntryValues(EntriesForTests[tc.entry])

			gotPath := t.TempDir()
			err := os.WriteFile(filepath.Join(gotPath, "parsed_values"), []byte(strings.Join(got, "\n")), 0600)
			require.NoError(t, err, "Setup: Failed to write the result")

			goldenPath := filepath.Join("testdata", t.Name(), "golden")
			testutils.CompareTreesWithFiltering(t, gotPath, goldenPath, Update)
		})
	}
}

func TestWriteFileWithUIDGID(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		uid     string
		gid     string
		content string

		readOnlyDir       bool
		pathAlreadyExists bool

		wantErr bool
	}{
		"write file with current user ownership": {},

		"error when invalid uid":                               {uid: "-150", wantErr: true},
		"error when invalid gid":                               {gid: "-150", wantErr: true},
		"error when writing on a dir with invalid permissions": {readOnlyDir: true, wantErr: true},
		"error when path already exists as a directory":        {pathAlreadyExists: true, wantErr: true},
	}

	u, err := user.Current()
	require.NoError(t, err, "Setup: failed to get current user")

	for name, tc := range tests {
		tc := tc
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			path := t.TempDir()

			uid := u.Uid
			if tc.uid != "" {
				uid = tc.uid
			}

			gid := u.Gid
			if tc.gid != "" {
				gid = tc.gid
			}

			if tc.readOnlyDir {
				testutils.MakeReadOnly(t, path)
			}

			iUID, err := strconv.Atoi(uid)
			require.NoError(t, err, "Setup: Failed to convert uid to int")
			iGID, err := strconv.Atoi(gid)
			require.NoError(t, err, "Setup: Failed to convert gid to int")

			filePath := filepath.Join(path, "test_write")

			if tc.pathAlreadyExists {
				err = os.MkdirAll(filePath, 0750)
				require.NoError(t, err, "Setup: Failed to set up pre existent directory for testing")

				t.Cleanup(func() {
					//nolint:errcheck // We created the folder for the test, so we know this function will not return an error.
					_ = os.Remove(filePath)
				})
			}

			err = writeFileWithUIDGID(filePath, iUID, iGID, "testing writeFileWithUIDGID file")
			if tc.wantErr {
				require.Error(t, err, "writeFileWithUIDGID should have returned an error but didn't")
				return
			}
			require.NoError(t, err, "writeFileWithUIDGID should not have returned an error but did")
			testutils.CompareTreesWithFiltering(t, path, filepath.Join("testdata", t.Name(), "golden"), Update)
		})
	}
}

func TestMain(m *testing.M) {
	flag.BoolVar(&Update, "update", false, "Update the golden files")
	flag.Parse()
	m.Run()
}
