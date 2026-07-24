# Crash Recovery System

## Overview

The app includes an automatic crash recovery system that saves your work when normal save operations fail. This ensures no data loss in case of system issues, permission problems, or unexpected errors.

## Pull Safety Copies

Pull safety copies are separate from crash recovery. If a file changes while a safe Pull is finishing, Astro Editor automatically retains the newer file version instead of deleting or overwriting it.

- At startup, the app checks the open project and shows a notice when retained copies need attention. The notice opens **Project Settings > Pull Safety Copies** directly.
- Project Settings lists each retained group with its date, files, and sizes. Copies from an interrupted or incompletely sealed Pull are marked **Needs attention** and shown first.
- **Restore copies** writes the saved versions into a new folder inside a destination chosen by the owner. It never overwrites project files, and the retained entry remains available after restore.
- **Delete permanently** is the only cleanup path. It requires explicit confirmation before removing the retained files and their recovery reference.

## How It Works

When a save operation fails, the system automatically:

1. **Saves Recovery Data**: Creates a JSON file with complete editor state (content + frontmatter)
2. **Saves Markdown File**: Creates a plain Markdown file with just the content for easy reading
3. **Logs Console Message**: Shows "Attempting to save recovery data..." in the console
4. **Creates Crash Report**: Saves debugging information for developers

## Recovery File Locations

### macOS

- **Recovery Files**: `~/Library/Application Support/is.danny.astroeditor/recovery/`
- **Crash Reports**: `~/Library/Application Support/is.danny.astroeditor/crash-reports/`

### Windows

- **Recovery Files**: `%APPDATA%\is.danny.astroeditor\recovery\`
- **Crash Reports**: `%APPDATA%\is.danny.astroeditor\crash-reports\`

## File Naming Convention

### Recovery Files

- **JSON**: `{timestamp}-{filename}.recovery.json` (complete state)
- **Markdown**: `{timestamp}-{filename}.recovery.md` (content only)

### Crash Reports

- **JSON**: `{timestamp}-crash.json` (debugging information)

## Recovery Process

1. **Find the Recovery Directory** (see locations above)
2. **Locate Your Files** by timestamp and filename
3. **Read the Markdown File** for quick content access
4. **Use the JSON File** to restore complete state including frontmatter

## JSON File Structure

```json
{
  "timestamp": "2024-01-15T10:30:00.000Z",
  "originalFilePath": "/path/to/original/file.md",
  "projectPath": "/path/to/project",
  "editorContent": "# Your content here...",
  "frontmatter": {
    "title": "Your title",
    "date": "2024-01-15",
    "draft": false
  },
  "fileName": "your-file.md",
  "collection": "posts"
}
```

## When Recovery is Triggered

Recovery files are created when:

- File save operations fail due to permissions
- Project directory is moved or unavailable
- External storage devices are disconnected
- System I/O errors occur
- Any other critical save failures

## Manual Recovery Steps

1. **Open Recovery Directory** using Finder (macOS) or File Explorer (Windows)
2. **Find Latest Recovery Files** by timestamp
3. **Copy Content** from the `.recovery.md` file
4. **Restore Frontmatter** from the `.recovery.json` file if needed
5. **Paste into New File** in the application

## Cleanup

Recovery files are not automatically deleted to prevent data loss. You can manually remove old recovery files when no longer needed.

## Technical Implementation

- **Backend**: Rust commands `save_recovery_data` and `save_crash_report`
- **Frontend**: Recovery manager in `src/lib/recovery/`
- **Integration**: Automatic triggering on save failures in store
- **Testing**: Comprehensive test coverage for recovery scenarios
