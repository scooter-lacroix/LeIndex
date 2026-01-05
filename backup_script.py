import os
import shutil
import datetime
import argparse
import sys

def create_backup(db_path: str, project_root: str, backup_dir: str):
    """
    Performs a complete backup of the existing SQLite database and application code.
    """
    timestamp = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    
    # Ensure backup directory exists
    os.makedirs(backup_dir, exist_ok=True)
    print(f"Backup directory '{backup_dir}' ensured.")

    # 1. Backup SQLite database
    db_filename = os.path.basename(db_path)
    db_backup_path = os.path.join(backup_dir, f"{db_filename}_{timestamp}.bak")
    
    try:
        if os.path.exists(db_path):
            shutil.copy2(db_path, db_backup_path)
            print(f"SQLite database backed up to: {db_backup_path}")
        else:
            print(f"Warning: SQLite database file not found at '{db_path}'. Skipping database backup.")
    except IOError as e:
        print(f"Error backing up database: {e}", file=sys.stderr)
        # Do not exit, continue with project backup if possible
    except Exception as e:
        print(f"An unexpected error occurred during database backup: {e}", file=sys.stderr)
        # Do not exit, continue with project backup if possible

    # 2. Create a compressed archive of the entire project directory
    archive_name = f"code_index_mcp_project_{timestamp}"
    archive_path = os.path.join(backup_dir, archive_name)
    
    try:
        print(f"Creating compressed archive of project directory '{project_root}'...")
        shutil.make_archive(archive_path, 'zip', project_root)
        print(f"Project directory archived to: {archive_path}.zip")
    except shutil.Error as e:
        print(f"Error creating project archive: {e}", file=sys.stderr)
        sys.exit(1) # Exit if project archive fails, as it's a critical part of the backup
    except Exception as e:
        print(f"An unexpected error occurred during project archiving: {e}", file=sys.stderr)
        sys.exit(1) # Exit if project archive fails

    print("\nBackup process completed.")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Perform a complete backup of the SQLite database and application code.")
    parser.add_argument("--db_path", type=str, 
                        default=os.path.join("data", "code_index.db"),
                        help="Path to the SQLite database file (default: data/code_index.db)")
    parser.add_argument("--project_root", type=str, 
                        default=".",
                        help="Path to the project root directory (default: current directory)")
    parser.add_argument("--backup_dir", type=str, 
                        default="backups",
                        help="Directory to store backups (default: backups/)")
    
    args = parser.parse_args()

    print(f"Starting backup with:")
    print(f"  Database Path: {args.db_path}")
    print(f"  Project Root: {args.project_root}")
    print(f"  Backup Directory: {args.backup_dir}")
    
    create_backup(args.db_path, args.project_root, args.backup_dir)