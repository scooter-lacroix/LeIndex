"""
Configuration Validation for LeIndex

This module provides validation for configuration values, ensuring they meet
minimum and maximum limits and follow expected formats.

Key Features:
- Comprehensive validation rules for all config parameters
- Min/max limit validation
- Type checking
- Clear error messages
"""

from typing import Any, Dict, List


class ValidationError(Exception):
    """Exception raised when configuration validation fails."""

    def __init__(self, message: str, field: str = None):
        """Initialize validation error.

        Args:
            message: Error message
            field: Optional field name that caused the error
        """
        self.field = field
        if field:
            super().__init__(f"{field}: {message}")
        else:
            super().__init__(message)


class ConfigValidator:
    """Validates configuration values and structure.

    This class provides:
    - Complete config validation
    - Individual field validation
    - Min/max limit checking
    - Type validation

    Example:
        >>> validator = ConfigValidator()
        >>> validator.validate_config(config_dict)
        >>> validator.validate_value('memory.total_budget_mb', 3072)
    """

    # Validation rules for all configuration parameters
    VALIDATION_RULES = {
        'version': {
            'type': str,
            'allowed_values': ['1.0', '2.0'],
        },
        'memory.total_budget_mb': {
            'type': int,
            'min': 512,  # 512MB minimum
            'max': 65536,  # 64GB maximum
        },
        'memory.global_index_mb': {
            'type': int,
            'min': 128,  # 128MB minimum
            'max': 8192,  # 8GB maximum
        },
        'memory.warning_threshold_percent': {
            'type': int,
            'min': 50,
            'max': 95,
        },
        'memory.prompt_threshold_percent': {
            'type': int,
            'min': 60,
            'max': 99,
        },
        'memory.emergency_threshold_percent': {
            'type': int,
            'min': 70,
            'max': 100,
        },
        'projects.estimated_mb': {
            'type': int,
            'min': 32,  # 32MB minimum
            'max': 4096,  # 4GB maximum
        },
        'projects.priority': {
            'type': str,
            'allowed_values': ['high', 'normal', 'low'],
        },
        'projects.max_file_size': {
            'type': int,
            'min': 1024,  # 1KB minimum
            'max': 1073741824,  # 1GB maximum
        },
        'performance.cache_enabled': {
            'type': bool,
        },
        'performance.cache_ttl_seconds': {
            'type': int,
            'min': 30,
            'max': 3600,
        },
        'performance.parallel_workers': {
            'type': int,
            'min': 1,
            'max': 32,
        },
        'performance.batch_size': {
            'type': int,
            'min': 10,
            'max': 500,
        },
    }

    # Threshold relationship constraints
    THRESHOLD_CONSTRAINTS = {
        'memory': {
            'warning_threshold_percent': {'must_be_lt': ['prompt_threshold_percent', 'emergency_threshold_percent']},
            'prompt_threshold_percent': {'must_be_lt': ['emergency_threshold_percent']},
            'emergency_threshold_percent': {'must_be_gt': ['warning_threshold_percent', 'prompt_threshold_percent']},
        }
    }

    def __init__(self):
        """Initialize the configuration validator."""
        pass

    def validate_config(self, config: Dict[str, Any]) -> None:
        """Validate complete configuration structure and values.

        Args:
            config: Configuration dictionary to validate

        Raises:
            ValidationError: If validation fails
        """
        if not isinstance(config, dict):
            raise ValidationError("Configuration must be a dictionary")

        # Validate top-level sections
        for section in ['memory', 'projects', 'performance']:
            if section not in config:
                raise ValidationError(f"Missing required section: {section}")

            if not isinstance(config[section], dict):
                raise ValidationError(f"Section '{section}' must be a dictionary")

        # Validate each field
        self._validate_section('memory', config['memory'])
        self._validate_section('projects', config['projects'])
        self._validate_section('performance', config['performance'])

        # Validate cross-field constraints
        self._validate_threshold_constraints(config['memory'])

    def validate_value(self, key: str, value: Any) -> None:
        """Validate a single configuration value.

        Args:
            key: Configuration key (e.g., 'memory.total_budget_mb')
            value: Value to validate

        Raises:
            ValidationError: If validation fails
        """
        if key not in self.VALIDATION_RULES:
            raise ValidationError(f"Unknown configuration key: {key}")

        rules = self.VALIDATION_RULES[key]

        # Type validation
        expected_type = rules['type']
        if not isinstance(value, expected_type):
            raise ValidationError(
                f"Invalid type for {key}: expected {expected_type.__name__}, got {type(value).__name__}",
                field=key
            )

        # Min/max validation
        if 'min' in rules and value < rules['min']:
            raise ValidationError(
                f"Value {value} is below minimum {rules['min']}",
                field=key
            )

        if 'max' in rules and value > rules['max']:
            raise ValidationError(
                f"Value {value} exceeds maximum {rules['max']}",
                field=key
            )

        # Allowed values validation
        if 'allowed_values' in rules and value not in rules['allowed_values']:
            raise ValidationError(
                f"Value '{value}' not in allowed values: {rules['allowed_values']}",
                field=key
            )

    def _validate_section(self, section_name: str, section: Dict[str, Any]) -> None:
        """Validate a configuration section.

        Args:
            section_name: Name of the section (e.g., 'memory')
            section: Section dictionary to validate

        Raises:
            ValidationError: If validation fails
        """
        for key, value in section.items():
            full_key = f"{section_name}.{key}"
            self.validate_value(full_key, value)

    def _validate_threshold_constraints(self, memory_config: Dict[str, Any]) -> None:
        """Validate threshold ordering constraints.

        Ensures: warning < prompt < emergency

        Args:
            memory_config: Memory configuration section

        Raises:
            ValidationError: If constraints are violated
        """
        warning = memory_config.get('warning_threshold_percent', 80)
        prompt = memory_config.get('prompt_threshold_percent', 93)
        emergency = memory_config.get('emergency_threshold_percent', 98)

        if warning >= prompt:
            raise ValidationError(
                f"warning_threshold_percent ({warning}) must be less than prompt_threshold_percent ({prompt})",
                field='memory.warning_threshold_percent'
            )

        if prompt >= emergency:
            raise ValidationError(
                f"prompt_threshold_percent ({prompt}) must be less than emergency_threshold_percent ({emergency})",
                field='memory.prompt_threshold_percent'
            )

        if warning >= emergency:
            raise ValidationError(
                f"warning_threshold_percent ({warning}) must be less than emergency_threshold_percent ({emergency})",
                field='memory.emergency_threshold_percent'
            )

        # Validate global_index_mb is reasonable relative to total_budget_mb
        total_budget = memory_config.get('total_budget_mb', 3072)
        global_index = memory_config.get('global_index_mb', 512)

        if global_index > total_budget * 0.5:
            raise ValidationError(
                f"global_index_mb ({global_index}) should not exceed 50% of total_budget_mb ({total_budget})",
                field='memory.global_index_mb'
            )

        if global_index < total_budget * 0.1:
            raise ValidationError(
                f"global_index_mb ({global_index}) should be at least 10% of total_budget_mb ({total_budget})",
                field='memory.global_index_mb'
            )

    def get_validation_rules(self) -> Dict[str, Dict[str, Any]]:
        """Get all validation rules.

        Returns:
            Dictionary of validation rules for all fields
        """
        return self.VALIDATION_RULES.copy()

    def get_field_rules(self, key: str) -> Dict[str, Any]:
        """Get validation rules for a specific field.

        Args:
            key: Configuration key

        Returns:
            Dictionary of validation rules for the field

        Raises:
            ValidationError: If key is not found
        """
        if key not in self.VALIDATION_RULES:
            raise ValidationError(f"Unknown configuration key: {key}")

        return self.VALIDATION_RULES[key].copy()

    def is_valid_value(self, key: str, value: Any) -> bool:
        """Check if a value is valid without raising an exception.

        Args:
            key: Configuration key
            value: Value to check

        Returns:
            True if value is valid, False otherwise
        """
        try:
            self.validate_value(key, value)
            return True
        except ValidationError:
            return False
