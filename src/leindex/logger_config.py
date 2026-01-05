import logging
import sys
from pythonjsonlogger import jsonlogger

def setup_logging():
    """
    Sets up centralized logging for the application to output structured JSON logs.

    IMPORTANT: Logs are directed to stderr, NOT stdout.
    The MCP stdio transport uses stdout exclusively for JSON-RPC protocol messages.
    Writing logs to stdout would break MCP communication.
    """
    log_level = logging.DEBUG
    
    # Create a logger instance
    logger = logging.getLogger('leindex')
    logger.setLevel(log_level)
    
    # Prevent adding multiple handlers if setup_logging is called multiple times
    if not logger.handlers:
        # Create a JSON formatter
        formatter = jsonlogger.JsonFormatter(
            '%(asctime)s %(levelname)s %(name)s %(message)s',
            rename_fields={'levelname': 'level', 'asctime': 'timestamp', 'name': 'logger'},
            json_ensure_ascii=False
        )
        
        # Define a filter to add default fields
        class DefaultFieldsFilter(logging.Filter):
            def filter(self, record):
                record.service = 'leindex'
                record.environment = 'development'
                return True
        
        # Add the filter to the logger
        logger.addFilter(DefaultFieldsFilter())

        # Create a stream handler for stderr
        # CRITICAL: MCP stdio transport uses stdout for JSON-RPC messages only
        # Writing logs to stdout would break MCP communication
        handler = logging.StreamHandler(sys.stderr)
        handler.setFormatter(formatter)
        logger.addHandler(handler)

        # Optionally, add a file handler for persistent logs
        # file_handler = logging.FileHandler('app.log')
        # file_handler.setFormatter(formatter)
        # logger.addHandler(file_handler)

        # Set root logger level to WARNING to avoid duplicate messages from libraries
        # unless explicitly configured by their own loggers.
        logging.getLogger().setLevel(logging.WARNING)
        
        # Set specific library log levels if needed
        logging.getLogger('uvicorn').setLevel(logging.INFO)
        logging.getLogger('uvicorn.access').setLevel(logging.INFO)
        logging.getLogger('elasticsearch').setLevel(logging.DEBUG)
        logging.getLogger('pika').setLevel(logging.WARNING)
        logging.getLogger('psycopg2').setLevel(logging.WARNING)

    return logger

# Initialize logging when this module is imported
logger = setup_logging()

if __name__ == "__main__":
    # Example usage
    logger.info("This is an info message from logger_config.py", extra={'event': 'startup', 'component': 'logging'})
    logger.warning("This is a warning message.", extra={'user_id': 123})
    try:
        1 / 0
    except ZeroDivisionError:
        logger.exception("An error occurred during division.", extra={'error_code': 'MATH_001'})
    
    # Test with different log levels
    logger.debug("This is a debug message (should not appear if level is INFO).")
    logger.error("This is an error message.")