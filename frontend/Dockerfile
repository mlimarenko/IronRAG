FROM busybox:1.37

COPY dist/spa /www

EXPOSE 3000
CMD ["httpd", "-f", "-v", "-p", "3000", "-h", "/www"]
