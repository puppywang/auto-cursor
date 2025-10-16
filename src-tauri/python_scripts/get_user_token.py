"""
Cursor Token Management Utility

This module provides basic token processing functionality for Cursor authentication.
No external server communication is performed.
"""

import os
from colorama import Fore, Style

# Define emoji constants
EMOJI = {
    'START': '🚀',
    'OAUTH': '🔑',
    'SUCCESS': '✅',
    'ERROR': '❌',
    'WAIT': '⏳',
    'INFO': 'ℹ️',
    'WARNING': '⚠️'
}

def process_token(cookie_value, translator=None):
    """Process and extract token from cookie value

    Args:
        cookie_value (str): The WorkosCursorSessionToken cookie value
        translator: Optional translator object

    Returns:
        str: The processed token
    """
    try:
        if not cookie_value:
            print(f"{Fore.YELLOW}{EMOJI['WARNING']} {translator.get('token.empty_cookie') if translator else 'Empty cookie value provided'}{Style.RESET_ALL}")
            return ""

        # Basic token extraction - split on :: or %3A%3A
        if '%3A%3A' in cookie_value:
            token = cookie_value.split('%3A%3A')[-1]
        elif '::' in cookie_value:
            token = cookie_value.split('::')[-1]
        else:
            token = cookie_value

        if token:
            print(f"{Fore.GREEN}{EMOJI['SUCCESS']} {translator.get('token.extracted') if translator else 'Token extracted successfully'}{Style.RESET_ALL}")
            return token
        else:
            print(f"{Fore.YELLOW}{EMOJI['WARNING']} {translator.get('token.no_token_found') if translator else 'No token found in cookie'}{Style.RESET_ALL}")
            return ""

    except Exception as e:
        print(f"{Fore.RED}{EMOJI['ERROR']} {translator.get('token.extraction_error', error=str(e)) if translator else f'Error extracting token: {str(e)}'}{Style.RESET_ALL}")
        return ""

def validate_token_format(token, translator=None):
    """Validate basic token format

    Args:
        token (str): The token to validate
        translator: Optional translator object

    Returns:
        bool: True if token format is valid
    """
    if not token:
        return False

    # Basic validation - tokens should be non-empty strings
    if len(token.strip()) == 0:
        return False

    # Check for basic patterns (this is a simple check)
    # Real validation would depend on Cursor's token format
    if len(token) < 10:  # Arbitrary minimum length
        print(f"{Fore.YELLOW}{EMOJI['WARNING']} {translator.get('token.too_short') if translator else 'Token appears to be too short'}{Style.RESET_ALL}")
        return False

    return True 